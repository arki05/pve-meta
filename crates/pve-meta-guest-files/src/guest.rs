//! The file operations inside a guest a sync needs, and the sync itself.
//!
//! [`Guest`] is the whole surface: probe, read, commit. A sync reads the
//! manifest, probes every path the plan needs, decides ([`crate::plan`]), and
//! only then writes, in one commit: the missing directories, then each file
//! written to its temp sibling and renamed over its target if the target is
//! still what the probe saw, each removal likewise. An operation that cannot
//! be done fails on its own and the others go ahead. The manifest is written
//! last, from what was done.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::entry::{Owner, GuestFiles};
use crate::manifest::{self, Manifest};
use crate::plan::{self, Action, Expect, Item, Node, Op, Plan};

/// What happened to one operation of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The file was no longer what the probe saw; not touched.
    Changed,
    /// Writing, owning, renaming or removing failed.
    Failed(String),
}

/// A guest's filesystem, as a sync uses it. Paths are absolute inside the
/// guest and valid ([`crate::entry::GuestPath`], or the manifest's).
pub trait Guest {
    /// `lstat` of every path, with the content hash of a regular file.
    fn probe(&mut self, paths: &[String]) -> Result<BTreeMap<String, Node>>;
    /// The content of a regular file of at most `max` bytes; `None` when
    /// there is none, or the path is a symlink. More than `max` is an error.
    fn read(&mut self, path: &str, max: usize) -> Result<Option<Vec<u8>>>;
    /// Creates `dirs`, parent first, as root `0755`, failing on a symlink or
    /// a non-directory in the way; then runs every operation in order, one
    /// outcome each.
    fn commit(&mut self, dirs: &[String], ops: &[Op]) -> Result<Vec<Outcome>>;
}

/// A plan, with what it was decided from.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub plan: Plan,
    /// The manifest as read; empty when there was none or it is not trusted.
    pub manifest: Manifest,
    /// Why the manifest in the guest is not trusted.
    pub warning: Option<String>,
}

/// Reads the manifest and probes the guest, and decides. Changes nothing.
pub fn inspect(g: &mut dyn Guest, files: &GuestFiles, force: bool) -> Result<Inspection> {
    let (manifest, warning) = match g.read(&manifest::path(), manifest::MAX_BYTES)? {
        None => (Manifest::default(), None),
        Some(bytes) => match Manifest::parse(&bytes) {
            Ok(m) => (m, None),
            Err(e) => {
                let why = format!("{}: {e}; nothing in it is trusted", manifest::path());
                (Manifest::default(), Some(why))
            }
        },
    };
    // Nothing wanted and nothing recorded: there is no path to look at.
    let nodes = if files.wants_files() || !manifest.is_empty() {
        let paths: Vec<String> = plan::probe_paths(files, &manifest).into_iter().collect();
        g.probe(&paths)?
    } else {
        BTreeMap::new()
    };
    let plan = plan::plan(files, &manifest, &nodes, force);
    Ok(Inspection { plan, manifest, warning })
}

/// One row of a sync's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub item: Item,
    /// For an item that writes: what became of it.
    pub outcome: Option<Outcome>,
}

/// What a sync did.
#[derive(Debug, Clone)]
pub struct Report {
    pub items: Vec<Done>,
    pub warning: Option<String>,
    /// The manifest has records after this sync.
    pub has_records: bool,
}

/// Runs one sync.
pub fn sync(g: &mut dyn Guest, files: &GuestFiles, force: bool) -> Result<Report> {
    let ins = inspect(g, files, force)?;
    let plan = &ins.plan;
    let mut outcomes = Vec::new();
    let mut manifest = ins.manifest.clone();
    if plan.changes(&ins.manifest) {
        outcomes = if plan.ops.is_empty() { Vec::new() } else { g.commit(&plan.dirs, &plan.ops)? };
        let done: Vec<bool> = outcomes.iter().map(|o| *o == Outcome::Done).collect();
        let after = plan.manifest_after(&ins.manifest, &done);
        if after != ins.manifest {
            let dirs = if plan.ops.is_empty() { plan.dirs.as_slice() } else { &[] };
            let op = Op::Place {
                path: manifest::path(),
                content: after.render(),
                mode: manifest::MODE,
                owner: Owner::ROOT,
                expect: Expect::Any,
            };
            if let [Outcome::Failed(why)] = g.commit(dirs, &[op])?.as_slice() {
                anyhow::bail!("writing {}: {why}", manifest::path());
            }
            manifest = after;
        }
    }
    let mut outcomes = outcomes.into_iter();
    let items = plan
        .items
        .iter()
        .map(|item| Done {
            item: item.clone(),
            outcome: if item.action.writes() { outcomes.next() } else { None },
        })
        .collect();
    Ok(Report { items, warning: ins.warning, has_records: !manifest.is_empty() })
}

/// The log line for one row: its action, and what became of it; `None` for a
/// row that is nothing to report.
pub fn describe(d: &Done) -> Option<String> {
    if matches!(d.item.action, Action::InSync | Action::Forget) {
        return None;
    }
    let path = d.item.path.as_ref().map(|p| p.as_str()).unwrap_or("-");
    let outcome = match &d.outcome {
        None => String::new(),
        Some(Outcome::Done) => ": done".into(),
        Some(Outcome::Changed) => ": not done, the file changed during the sync".into(),
        Some(Outcome::Failed(why)) => format!(": failed: {why}"),
    };
    Some(format!("entry {:?} {path}: {}{outcome}", d.item.entry, d.item.action))
}

#[cfg(test)]
pub mod fake {
    //! A guest filesystem in memory, answering as the `pct` scripts do.

    use std::collections::BTreeSet;

    use super::*;
    use anyhow::bail;
    use pve_meta_core::digest::digest;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum F {
        Dir { uid: u32, mode: u32 },
        File { content: Vec<u8>, uid: u32, gid: u32, mode: u32 },
        Symlink,
    }

    #[derive(Default)]
    pub struct Fake {
        pub fs: BTreeMap<String, F>,
        /// Paths whose operation fails.
        pub fails: BTreeSet<String>,
        /// Content an edit racing the sync puts in place before the commit.
        pub race: Option<(String, String)>,
    }

    impl Fake {
        pub fn new() -> Fake {
            let mut f = Fake::default();
            f.fs.insert("/etc".into(), F::Dir { uid: 0, mode: 0o755 });
            f
        }

        pub fn put(&mut self, path: &str, content: &str) {
            let file = F::File { content: content.into(), uid: 0, gid: 0, mode: 0o644 };
            self.fs.insert(path.into(), file);
        }

        pub fn text(&self, path: &str) -> Option<String> {
            match self.fs.get(path) {
                Some(F::File { content, .. }) => Some(String::from_utf8(content.clone()).unwrap()),
                _ => None,
            }
        }
    }

    impl Guest for Fake {
        fn probe(&mut self, paths: &[String]) -> Result<BTreeMap<String, Node>> {
            let node = |f: Option<&F>| match f {
                None => Node::Missing,
                Some(F::Symlink) => Node::Symlink,
                Some(F::Dir { .. }) => Node::Dir,
                Some(F::File { content, uid, gid, mode }) => {
                    Node::File { uid: *uid, gid: *gid, mode: *mode, sha256: digest(content) }
                }
            };
            Ok(paths.iter().map(|p| (p.clone(), node(self.fs.get(p)))).collect())
        }

        fn read(&mut self, path: &str, max: usize) -> Result<Option<Vec<u8>>> {
            match self.fs.get(path) {
                Some(F::File { content, .. }) if content.len() > max => bail!("too large"),
                Some(F::File { content, .. }) => Ok(Some(content.clone())),
                _ => Ok(None),
            }
        }

        fn commit(&mut self, dirs: &[String], ops: &[Op]) -> Result<Vec<Outcome>> {
            for d in dirs {
                if !matches!(
                    self.fs.entry(d.clone()).or_insert(F::Dir { uid: 0, mode: 0o755 }),
                    F::Dir { .. }
                ) {
                    bail!("{d} is in the way");
                }
            }
            if let Some((path, content)) = self.race.take() {
                self.put(&path, &content);
            }
            let mut out = Vec::new();
            for op in ops {
                let path = op.path();
                let sha = match self.fs.get(path) {
                    Some(F::File { content, .. }) => Some(digest(content)),
                    _ => None,
                };
                let as_expected = match op {
                    Op::Place { expect: Expect::Missing, .. } => !self.fs.contains_key(path),
                    Op::Place { expect: Expect::Sha256(s), .. } => sha.as_ref() == Some(s),
                    Op::Place { expect: Expect::Any, .. } => true,
                    Op::Remove { sha256, .. } => sha.is_none() || sha.as_ref() == Some(sha256),
                };
                out.push(if !as_expected {
                    Outcome::Changed
                } else if self.fails.contains(path) {
                    Outcome::Failed("injected".into())
                } else {
                    match op {
                        Op::Place { content, mode, owner, .. } => {
                            let file = F::File {
                                content: content.clone(),
                                uid: owner.uid,
                                gid: owner.gid,
                                mode: *mode,
                            };
                            self.fs.insert(path.into(), file);
                        }
                        Op::Remove { .. } => {
                            self.fs.remove(path);
                        }
                    }
                    Outcome::Done
                });
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, F};
    use super::*;
    use pve_meta_core::format::{self, Format};

    fn guest_files(yaml: &str) -> GuestFiles {
        GuestFiles::of_document(&format::parse(Format::Yaml, yaml).unwrap())
    }

    const DOC: &str = "llm:\n  swap: {port: 8080}\nproxy: {host: web}\nguest-files:\n  \
                       swap: {view: llm.swap, path: llm/swap.yaml}\n  \
                       proxy: {view: proxy, path: /etc/traefik/dynamic.json, format: json, mode: \"0640\", owner: \"0:33\"}\n";
    const SWAP: &str = "/etc/pve-meta/llm/swap.yaml";

    fn actions(r: &Report) -> Vec<(Action, Option<Outcome>)> {
        r.items.iter().map(|d| (d.item.action.clone(), d.outcome.clone())).collect()
    }

    fn all_in_sync(r: &Report) -> bool {
        r.items.iter().all(|d| d.item.action == Action::InSync)
    }

    #[test]
    fn guest_files_update_and_remove() {
        let mut g = Fake::new();
        let r = sync(&mut g, &guest_files(DOC), false).unwrap();
        let created = (Action::Create, Some(Outcome::Done));
        assert_eq!(actions(&r), [created.clone(), created]);
        assert_eq!(g.text(SWAP).unwrap(), "port: 8080\n");
        assert_eq!(g.text("/etc/traefik/dynamic.json").unwrap(), "{\n  \"host\": \"web\"\n}\n");
        assert!(matches!(
            g.fs.get("/etc/traefik/dynamic.json"),
            Some(F::File { mode: 0o640, gid: 33, .. })
        ));
        assert!(matches!(g.fs.get("/etc/traefik"), Some(F::Dir { uid: 0, mode: 0o755 })));
        assert!(matches!(
            g.fs.get("/etc/pve-meta/.guest-files"),
            Some(F::File { mode: 0o600, uid: 0, .. })
        ));
        assert!(all_in_sync(&sync(&mut g, &guest_files(DOC), false).unwrap()));

        let r = sync(&mut g, &guest_files(&DOC.replace("8080", "9090")), false).unwrap();
        assert_eq!(actions(&r)[0], (Action::Update, Some(Outcome::Done)));
        assert!(describe(&r.items[0]).unwrap().ends_with("update: done"));

        let r = sync(&mut g, &guest_files("llm: {}\n"), false).unwrap();
        assert!(actions(&r).iter().all(|a| *a == (Action::Delete, Some(Outcome::Done))));
        assert!(g.text(SWAP).is_none() && !r.has_records);
        assert!(g.fs.contains_key("/etc/traefik"));
    }

    #[test]
    fn local_edits_and_unmanaged_files() {
        let mut g = Fake::new();
        g.fs.insert("/etc/traefik".into(), F::Dir { uid: 0, mode: 0o755 });
        g.put("/etc/traefik/dynamic.json", "the distribution's own\n");
        let r = sync(&mut g, &guest_files(DOC), false).unwrap();
        assert_eq!(actions(&r)[1].0, Action::KeptLocalEdit);
        assert!(describe(&r.items[1]).unwrap().ends_with("local edit kept"));
        // Removing the entry never deletes a file that was not ours.
        sync(&mut g, &guest_files("a: 1\n"), false).unwrap();
        assert_eq!(g.text("/etc/traefik/dynamic.json").unwrap(), "the distribution's own\n");

        g.put(SWAP, "port: 1\n");
        let r = sync(&mut g, &guest_files(DOC), false).unwrap();
        assert_eq!(actions(&r)[0].0, Action::KeptLocalEdit);
        let r = sync(&mut g, &guest_files(DOC), true).unwrap();
        assert!(actions(&r).iter().all(|a| *a == (Action::Overwrite, Some(Outcome::Done))));
        assert_eq!(g.text(SWAP).unwrap(), "port: 8080\n");
        // Adopted: the entry's removal now deletes it.
        let r = sync(&mut g, &guest_files("a: 1\n"), false).unwrap();
        assert!(actions(&r).iter().all(|a| a.0 == Action::Delete));
        assert!(g.text("/etc/traefik/dynamic.json").is_none());
    }

    #[test]
    fn a_failed_or_raced_item_does_not_block_the_others() {
        let mut g = Fake::new();
        sync(&mut g, &guest_files(DOC), false).unwrap();
        let changed = DOC.replace("8080", "9090").replace("web", "proxy");
        g.fails.insert(SWAP.into());
        let r = sync(&mut g, &guest_files(&changed), false).unwrap();
        assert!(matches!(actions(&r)[0].1, Some(Outcome::Failed(_))));
        assert_eq!(actions(&r)[1], (Action::Update, Some(Outcome::Done)));
        assert_eq!(g.text(SWAP).unwrap(), "port: 8080\n");
        g.fails.clear();
        let r = sync(&mut g, &guest_files(&changed), false).unwrap();
        assert_eq!(actions(&r)[0], (Action::Update, Some(Outcome::Done)));
        assert!(all_in_sync(&sync(&mut g, &guest_files(&changed), false).unwrap()));

        // An edit between the probe and the commit wins, and is then a local edit.
        g.race = Some((SWAP.into(), "raced\n".into()));
        let r = sync(&mut g, &guest_files(DOC), false).unwrap();
        assert_eq!(actions(&r)[0], (Action::Update, Some(Outcome::Changed)));
        assert_eq!(g.text(SWAP).unwrap(), "raced\n");
        let r = sync(&mut g, &guest_files(DOC), false).unwrap();
        assert_eq!(actions(&r)[0].0, Action::KeptLocalEdit);
    }

    #[test]
    fn an_untrusted_manifest_deletes_nothing_and_matching_files_are_adopted() {
        let mut g = Fake::new();
        sync(&mut g, &guest_files(DOC), false).unwrap();
        g.put("/etc/pve-meta/.guest-files", "garbage");
        let r = sync(&mut g, &guest_files("llm: {}\n"), false).unwrap();
        assert!(r.items.is_empty() && r.warning.is_some());
        assert!(g.text(SWAP).is_some());
        assert!(all_in_sync(&sync(&mut g, &guest_files(DOC), false).unwrap()));
        assert!(g.text("/etc/pve-meta/.guest-files").unwrap().contains("swap.yaml"));

        g.put("/etc/pve-meta/.guest-files", &"x".repeat(manifest::MAX_BYTES + 1));
        assert!(sync(&mut g, &guest_files(DOC), false).is_err());
    }

    #[test]
    fn a_guest_without_guest_files_is_not_written_to() {
        let mut g = Fake::new();
        let r = sync(&mut g, &GuestFiles::Nothing, false).unwrap();
        assert!(r.items.is_empty() && !r.has_records);
        let r = sync(&mut g, &guest_files("guest-files:\n  t: {view: a}\n"), false).unwrap();
        assert!(matches!(r.items[0].item.action, Action::Refused(_)));
        assert!(!g.fs.contains_key("/etc/pve-meta"));
    }
}
