//! [`Guest`] over `pct exec`: every read, check and write inside the guest is
//! one `pct exec` of a generated POSIX shell script fed to `/bin/sh -s` on
//! stdin, file content included, so nothing is ever pushed from a host file
//! and no call leaves a task-log row.
//!
//! A `pct` call starts a Perl interpreter, so the calls are batched: a sync
//! that changes nothing is two (read the manifest, probe every path), or one
//! when nothing is wanted or recorded; one that writes adds a commit and a
//! manifest write. The scripts run as root inside the guest's namespaces, so
//! ids and paths are the guest's own. They need `/bin/sh`, `stat -c`,
//! `sha256sum`, `base64 -d`, `head`, `cut`, `mv`, `rm`, `mkdir`, `chown` and
//! `chmod`, which coreutils and busybox both provide; a guest without them
//! fails its sync with that said.
//!
//! A broken or hung container cannot wedge or bloat the daemon: a call's
//! stdout is capped at [`MAX_OUTPUT`] and its stderr at [`STDERR_CAP`], and a
//! call past either, or past [`TIMEOUT`], is killed with its whole process
//! group and is an error. Text from the guest is always `{:?}`-formatted where
//! it is shown, so it cannot break a log line onto more than one.
//!
//! Every path handed to a script is a [`crate::entry::GuestPath`] or the
//! manifest's, whose charset has no quote, space or glob character, and is
//! single-quoted anyway.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::entry::{mode_text, temp_for};
use crate::guest::{Guest, Outcome};
use crate::plan::{Expect, Node, Op};

/// How long one `pct` call may take before its process group is killed.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// The most stdout any call may produce.
pub const MAX_OUTPUT: usize = 4 * 1024 * 1024;

/// The most stderr any call may produce.
pub const STDERR_CAP: usize = 64 * 1024;

/// Captured result of a finished command.
#[derive(Debug)]
pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

fn capped<R: Read + Send + 'static>(
    mut r: R,
    cap: usize,
    over: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) if buf.len() + n > cap => {
                    over.store(true, Ordering::Relaxed);
                    break;
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        buf
    })
}

/// Runs `program args` in its own process group, with `stdin` (or none),
/// killing the group after `timeout` or once stdout passes [`MAX_OUTPUT`] or
/// stderr [`STDERR_CAP`].
pub fn run(
    program: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let over = Arc::new(AtomicBool::new(false));
    let out = capped(child.stdout.take().expect("piped"), MAX_OUTPUT, over.clone());
    let err = capped(child.stderr.take().expect("piped"), STDERR_CAP, over.clone());
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let kill = |child: &mut std::process::Child| {
        // SAFETY: kill(2) on the negated id of the group the child leads.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
    };
    let deadline = Instant::now() + timeout;
    let what = format!("{program} {}", args.first().unwrap_or(&""));
    let status = loop {
        if over.load(Ordering::Relaxed) {
            kill(&mut child);
            bail!("{what}: more output than allowed, killed");
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            bail!("{what}: no answer after {}s, killed", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if over.load(Ordering::Relaxed) {
        bail!("{what}: more output than allowed");
    }
    Ok(Output {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
    })
}

/// A container on this node.
pub struct Pct {
    pub vmid: u32,
}

const PRELUDE: &str = "set -u
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH LC_ALL=C
for t in stat sha256sum base64 head cut mv rm mkdir chown chmod; do
  command -v \"$t\" >/dev/null 2>&1 || { echo \"'$t' is not in the guest\" >&2; exit 90; }
done
";

/// `probe`: one line per path, `L`, `M`, `O`, `D` or
/// `F <uid> <gid> <mode> <sha256>`.
const PROBE: &str = "p() {
  if [ -L \"$1\" ]; then echo L
  elif [ ! -e \"$1\" ]; then echo M
  elif [ -d \"$1\" ]; then echo D
  elif [ -f \"$1\" ]; then
    s=$(stat -c '%u %g %a' \"$1\") || exit 91
    h=$(sha256sum < \"$1\" | cut -d' ' -f1) || exit 91
    echo \"F $s $h\"
  else echo O
  fi
}
";

/// `commit`: `d <dir>` makes a directory or ends the script; `w <i> <path>
/// <temp> <expect> <mode> <uid:gid>` writes base64 content from its stdin to a
/// new `<temp>`, owns it, and renames it over `<path>` if that is still
/// `<expect>` (`-` missing, `*` anything, or a sha256); `rmf <i> <path> <sha>`
/// removes. Each prints `<i> done`, `<i> changed` or `<i> failed <what>`.
const COMMIT: &str = "d() {
  if [ -L \"$1\" ]; then echo \"$1 is a symlink\" >&2; exit 92; fi
  if [ -d \"$1\" ]; then return 0; fi
  if [ -e \"$1\" ]; then echo \"$1 is not a directory\" >&2; exit 92; fi
  mkdir -m 0755 \"$1\" || exit 92
}
h() { sha256sum < \"$1\" | cut -d' ' -f1; }
w() {
  rm -f \"$3\"
  if ! ( umask 077; base64 -d > \"$3\" ) 2>/dev/null; then rm -f \"$3\"; echo \"$1 failed write\"; return 0; fi
  if ! chown \"$6\" \"$3\" 2>/dev/null || ! chmod \"$5\" \"$3\"; then rm -f \"$3\"; echo \"$1 failed owner or mode\"; return 0; fi
  case \"$4\" in
    '*') ;;
    -) if [ -e \"$2\" ] || [ -L \"$2\" ]; then rm -f \"$3\"; echo \"$1 changed\"; return 0; fi ;;
    *) if [ -L \"$2\" ] || [ ! -f \"$2\" ] || [ \"$(h \"$2\")\" != \"$4\" ]; then rm -f \"$3\"; echo \"$1 changed\"; return 0; fi ;;
  esac
  if ! mv -f \"$3\" \"$2\"; then rm -f \"$3\"; echo \"$1 failed rename\"; return 0; fi
  echo \"$1 done\"
}
rmf() {
  if [ -L \"$2\" ]; then echo \"$1 changed\"; return 0; fi
  if [ ! -e \"$2\" ]; then echo \"$1 done\"; return 0; fi
  if [ ! -f \"$2\" ] || [ \"$(h \"$2\")\" != \"$3\" ]; then echo \"$1 changed\"; return 0; fi
  if ! rm -f \"$2\"; then echo \"$1 failed removal\"; return 0; fi
  echo \"$1 done\"
}
";

/// Ends every heredoc of content; not in the base64 alphabet.
const EOF_MARK: &str = "PVE_META_GUEST_FILES_EOF";

fn quote(s: &str) -> String {
    debug_assert!(!s.contains('\''));
    format!("'{s}'")
}

/// Standard base64 in lines of 76, for `base64 -d`.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (i, chunk) in bytes.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push('\n');
        }
        let n =
            chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The probe script for `paths`.
pub fn probe_script(paths: &[String]) -> String {
    let mut s = format!("{PRELUDE}{PROBE}");
    for p in paths {
        let _ = writeln!(s, "p {}", quote(p));
    }
    s
}

/// Reads a probe's output, one line per path in order.
pub fn parse_probe(paths: &[String], stdout: &[u8]) -> Result<BTreeMap<String, Node>> {
    let text = std::str::from_utf8(stdout).context("probe output is not text")?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != paths.len() {
        bail!("probe answered {} lines for {} paths", lines.len(), paths.len());
    }
    let num = |t: &str, radix: u32| {
        u32::from_str_radix(t, radix).map_err(|_| anyhow!("bad number {t:?}"))
    };
    let mut out = BTreeMap::new();
    for (path, line) in paths.iter().zip(lines) {
        let f: Vec<&str> = line.split(' ').collect();
        let node = match f.as_slice() {
            ["L"] => Node::Symlink,
            ["M"] => Node::Missing,
            ["O"] => Node::Other,
            ["D"] => Node::Dir,
            ["F", uid, gid, mode, sha]
                if sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                let (uid, gid, mode) = (num(uid, 10)?, num(gid, 10)?, num(mode, 8)?);
                Node::File { uid, gid, mode, sha256: sha.to_ascii_lowercase() }
            }
            _ => bail!("probe of {path}: unreadable answer {line:?}"),
        };
        out.insert(path.clone(), node);
    }
    Ok(out)
}

/// The commit script: `dirs`, then `ops` with their content inline.
pub fn commit_script(dirs: &[String], ops: &[Op]) -> String {
    let mut s = format!("{PRELUDE}{COMMIT}");
    for d in dirs {
        let _ = writeln!(s, "d {}", quote(d));
    }
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::Place { path, content, mode, owner, expect } => {
                let expect = match expect {
                    Expect::Missing => "-".to_string(),
                    Expect::Any => "*".to_string(),
                    Expect::Sha256(sha) => sha.clone(),
                };
                let _ = writeln!(
                    s,
                    "w {i} {} {} {} {} {owner} <<'{EOF_MARK}'\n{}\n{EOF_MARK}",
                    quote(path),
                    quote(&temp_for(path)),
                    quote(&expect),
                    mode_text(*mode),
                    base64(content),
                );
            }
            Op::Remove { path, sha256 } => {
                let _ = writeln!(s, "rmf {i} {} {}", quote(path), quote(sha256));
            }
        }
    }
    s
}

/// Reads a commit's output: one line per operation, in order.
pub fn parse_commit(n: usize, stdout: &[u8]) -> Result<Vec<Outcome>> {
    let text = std::str::from_utf8(stdout).context("commit output is not text")?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let rest = line.strip_prefix(&format!("{i} "));
        out.push(match rest {
            Some("done") => Outcome::Done,
            Some("changed") => Outcome::Changed,
            Some(r) if r.starts_with("failed ") => Outcome::Failed(r[7..].to_string()),
            _ => bail!("commit: unreadable answer {line:?}"),
        });
    }
    if out.len() != n {
        bail!("commit answered {} of {n} operations", out.len());
    }
    Ok(out)
}

impl Pct {
    fn exec(&self, script: &str) -> Result<Output> {
        let vmid = self.vmid.to_string();
        run("pct", &["exec", &vmid, "--", "/bin/sh", "-s"], Some(script.as_bytes()), TIMEOUT)
    }

    fn exec_ok(&self, what: &str, script: &str) -> Result<Output> {
        let out = self.exec(script)?;
        if out.status != 0 {
            bail!("{what} in guest {} failed (exit {}): {:?}", self.vmid, out.status, out.stderr);
        }
        Ok(out)
    }
}

impl Guest for Pct {
    fn probe(&mut self, paths: &[String]) -> Result<BTreeMap<String, Node>> {
        let out = self.exec_ok("probe", &probe_script(paths))?;
        parse_probe(paths, &out.stdout)
    }

    fn read(&mut self, path: &str, max: usize) -> Result<Option<Vec<u8>>> {
        let script = format!(
            "{PRELUDE}if [ -L {p} ] || [ ! -f {p} ]; then exit 3; fi\nexec head -c {} {p}\n",
            max + 1,
            p = quote(path)
        );
        let out = self.exec(&script)?;
        match out.status {
            0 if out.stdout.len() > max => {
                bail!("{path} in guest {} is above {max} bytes", self.vmid)
            }
            0 => Ok(Some(out.stdout)),
            3 => Ok(None),
            s => bail!("reading {path} in guest {} failed (exit {s}): {:?}", self.vmid, out.stderr),
        }
    }

    fn commit(&mut self, dirs: &[String], ops: &[Op]) -> Result<Vec<Outcome>> {
        let out = self.exec_ok("commit", &commit_script(dirs, ops))?;
        parse_commit(ops.len(), &out.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Owner;

    #[test]
    fn parsing_and_encoding() {
        let paths: Vec<String> =
            ["/a", "/b", "/c", "/d", "/e"].into_iter().map(String::from).collect();
        let sha = "a".repeat(64);
        let text = format!("D\nF 0 33 640 {sha}\nL\nO\nM\n");
        let nodes = parse_probe(&paths, text.as_bytes()).unwrap();
        assert_eq!(nodes["/a"], Node::Dir);
        assert_eq!(nodes["/b"], Node::File { uid: 0, gid: 33, mode: 0o640, sha256: sha });
        assert_eq!(
            (&nodes["/c"], &nodes["/d"], &nodes["/e"]),
            (&Node::Symlink, &Node::Other, &Node::Missing)
        );
        for bad in ["", "D 0\n", "F 0 0 644 short\n", "D 0 9\n", "M\nM\n", "\u{1b}]0;x\n"] {
            let err = parse_probe(&paths[..1], bad.as_bytes()).unwrap_err().to_string();
            assert!(!err.chars().any(char::is_control), "{err:?}");
        }
        assert_eq!(
            parse_commit(3, b"0 done\n1 changed\n2 failed owner or mode\n").unwrap(),
            [Outcome::Done, Outcome::Changed, Outcome::Failed("owner or mode".into())]
        );
        for bad in [&b"0 done\n"[..], b"1 done\n0 done\n", b"0 maybe\n1 done\n"] {
            assert!(parse_commit(2, bad).is_err());
        }
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(&[0u8; 60]).lines().map(str::len).collect::<Vec<_>>(), [76, 4]);
    }

    #[test]
    fn run_caps_output_feeds_stdin_and_kills_on_timeout() {
        let out = run("sh", &["-s"], Some(b"echo from-stdin\n"), TIMEOUT).unwrap();
        assert_eq!(out.stdout, b"from-stdin\n");
        let flood = run(
            "sh",
            &["-c", "while :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done"],
            None,
            TIMEOUT,
        );
        assert!(flood.unwrap_err().to_string().contains("more output"));
        let err_flood = run("sh", &["-c", "while :; do echo xxxxxxxx >&2; done"], None, TIMEOUT);
        assert!(err_flood.unwrap_err().to_string().contains("more output"));
        let started = Instant::now();
        let slow = run("sh", &["-c", "sleep 30 & sleep 30"], None, Duration::from_millis(300));
        assert!(slow.unwrap_err().to_string().contains("killed"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// The scripts against a real `/bin/sh -s`, as root, in a scratch
    /// directory under `/run`: staging, owning, the hash-checked rename and
    /// removal, directory creation, and the probe's view of what it made.
    #[test]
    fn scripts_run_under_a_real_shell() {
        let have = |t: &str| {
            Command::new("sh")
                .args(["-c", &format!("command -v {t}")])
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let gnu_stat =
            Command::new("stat").args(["-c", "%a", "/"]).output().is_ok_and(|o| o.status.success());
        if !have("sha256sum") || !have("base64") || !gnu_stat || unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: needs root, stat -c, sha256sum and base64");
            return;
        }
        let root = format!("/run/pve-meta-guest-files-test-{}", std::process::id());
        let sh = |script: &str| {
            let out = run("sh", &["-s"], Some(script.as_bytes()), TIMEOUT).unwrap();
            assert_eq!(out.status, 0, "{}", out.stderr);
            out
        };
        let commit = |dirs: &[String], ops: &[Op]| {
            parse_commit(ops.len(), &sh(&commit_script(dirs, ops)).stdout).unwrap()
        };
        let place = |path: &str, content: &[u8], expect: Expect| Op::Place {
            path: path.into(),
            content: content.to_vec(),
            mode: 0o640,
            owner: Owner { uid: 1000, gid: 33 },
            expect,
        };
        let probe =
            |paths: &[String]| parse_probe(paths, &sh(&probe_script(paths)).stdout).unwrap();

        let dir = format!("{root}/conf");
        let file = format!("{dir}/a.yaml");
        let binary: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let got = commit(
            &[root.clone(), dir.clone()],
            &[
                place(&file, b"one\n", Expect::Missing),
                place(&format!("{dir}/bin"), &binary, Expect::Missing),
                place(&format!("{root}/none/x"), b"x", Expect::Missing),
                place(&file, b"two\n", Expect::Missing),
            ],
        );
        assert_eq!(got[..2], [Outcome::Done, Outcome::Done]);
        assert!(matches!(got[2], Outcome::Failed(_)), "{got:?}");
        assert_eq!(got[3], Outcome::Changed);
        assert_eq!(std::fs::read(&file).unwrap(), b"one\n");
        assert_eq!(std::fs::read(format!("{dir}/bin")).unwrap(), binary);
        let paths = vec![dir.clone(), file.clone()];
        let nodes = probe(&paths);
        assert_eq!(nodes[&dir], Node::Dir);
        {
            use std::os::unix::fs::MetadataExt as _;
            let m = std::fs::metadata(&dir).unwrap();
            assert_eq!((m.uid(), m.mode() & 0o7777), (0, 0o755));
        }
        let one = pve_meta_core::digest::digest(b"one\n");
        assert_eq!(
            nodes[&file],
            Node::File { uid: 1000, gid: 33, mode: 0o640, sha256: one.clone() }
        );
        assert!(!std::fs::read_dir(&dir).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp")));

        let got = commit(
            &[],
            &[
                place(&file, b"three\n", Expect::Sha256("0".repeat(64))),
                place(&file, b"three\n", Expect::Sha256(one.clone())),
                Op::Remove { path: format!("{dir}/bin"), sha256: one.clone() },
            ],
        );
        assert_eq!(got, [Outcome::Changed, Outcome::Done, Outcome::Changed]);
        assert_eq!(std::fs::read(&file).unwrap(), b"three\n");

        // A directory a service user owns is written into like any other.
        std::os::unix::fs::chown(&dir, Some(1000), None).unwrap();
        assert_eq!(probe(std::slice::from_ref(&dir))[&dir], Node::Dir);
        let owned = format!("{dir}/owned");
        assert_eq!(commit(&[], &[place(&owned, b"x", Expect::Missing)]), [Outcome::Done]);

        // Directory creation refuses a symlink in the way.
        let link = format!("{root}/link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let out =
            run("sh", &["-s"], Some(commit_script(&[link], &[]).as_bytes()), TIMEOUT).unwrap();
        assert_eq!(out.status, 92);

        let three = pve_meta_core::digest::digest(b"three\n");
        assert_eq!(
            commit(&[], &[Op::Remove { path: file.clone(), sha256: three }]),
            [Outcome::Done]
        );
        assert!(!std::path::Path::new(&file).exists());

        // A read above its cap is caught by the size, not by a large allocation.
        std::fs::write(format!("{root}/big"), vec![b'x'; 5000]).unwrap();
        let out = sh(&format!("{PRELUDE}exec head -c 4001 '{root}/big'\n"));
        assert_eq!(out.stdout.len(), 4001);
        let _ = std::fs::remove_dir_all(&root);
    }
}
