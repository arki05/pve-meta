//! `pve-meta-publish`: the daemon and the operator's commands, one binary.

use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use pve_meta_publish::daemon::{Daemon, Timing};
use pve_meta_publish::entry;
use pve_meta_publish::guest::{self, Outcome};
use pve_meta_publish::lock::GuestLock;
use pve_meta_publish::node::{self, Kind};
use pve_meta_publish::pct::Pct;
use pve_meta_publish::plan::Action;

const USAGE: &str = "usage: pve-meta-publish daemon
       pve-meta-publish sync <vmid> [--force]
       pve-meta-publish status [<vmid>]

  daemon  the loop the systemd unit runs: sync this node's containers when their
          document changes, when they start, and every ten minutes
  sync    sync one running container now; --force overwrites its local edits once
  status  per entry what a sync would do, checked in the guest now; every
          container on this node with a 'publish' key by default

Writes go to /etc/pve-meta inside the container for a relative path, to the path
itself for an absolute one. Only files recorded in /etc/pve-meta/.published are
ever replaced or deleted without local_edits: overwrite or --force.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match argv.as_slice() {
        ["daemon"] => Daemon::run(Timing::default()).map(|_| true),
        ["sync", vmid] => vmid_arg(vmid).and_then(|v| sync(v, false)),
        ["sync", vmid, "--force"] | ["sync", "--force", vmid] => {
            vmid_arg(vmid).and_then(|v| sync(v, true))
        }
        ["status"] => status(None),
        ["status", vmid] => vmid_arg(vmid).and_then(|v| status(Some(v))),
        [] | ["help" | "--help" | "-h"] => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("pve-meta-publish: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn vmid_arg(s: &str) -> Result<u32> {
    s.parse::<u32>().ok().filter(|v| *v > 0).with_context(|| format!("{s:?} is not a vmid"))
}

/// Why a container on this node cannot be synced now (stopped, locked), or
/// `None`; an error for anything that is not a container on this node.
fn not_syncable(vmid: u32) -> Result<Option<String>> {
    let here = node::nodename()?;
    let Some(g) = node::vmlist()?.remove(&vmid) else { bail!("{vmid} is not a guest") };
    if g.kind != Kind::Lxc {
        bail!("{vmid} is not a container; 'publish' is for containers only");
    }
    if g.node != here {
        bail!("{vmid} is on node {}; run this there", g.node);
    }
    if !node::active_containers()?.contains_key(&vmid) {
        return Ok(Some("stopped".into()));
    }
    Ok(node::container_lock(&here, vmid)?.map(|l| format!("locked ({l})")))
}

fn sync(vmid: u32, force: bool) -> Result<bool> {
    if let Some(why) = not_syncable(vmid)? {
        bail!("{vmid} is {why}");
    }
    let publication = entry::read_publication(&entry::open_store(), vmid)?;
    let _lock = GuestLock::take(vmid, true)?;
    let report = guest::sync(&mut Pct { vmid }, &publication, force)?;
    if let Some(w) = &report.warning {
        println!("warning: {w}");
    }
    let mut ok = true;
    let mut said = false;
    for d in &report.items {
        ok &= !matches!(d.item.action, Action::Refused(_));
        ok &= matches!(d.outcome, None | Some(Outcome::Done));
        if let Some(line) = guest::describe(d) {
            println!("{line}");
            said = true;
        }
    }
    if !said {
        println!("{vmid}: in sync");
    }
    Ok(ok)
}

fn row(vmid: u32, entry: &str, path: &str, state: &str) {
    println!("{vmid:<8} {entry:<20} {path:<44} {state}");
}

fn status(only: Option<u32>) -> Result<bool> {
    let store = entry::open_store();
    let vmids: Vec<u32> = match only {
        Some(v) => vec![v],
        None => {
            let here = node::nodename()?;
            let list = node::vmlist()?;
            list.into_iter()
                .filter(|(_, g)| g.node == here && g.kind == Kind::Lxc)
                .map(|(v, _)| v)
                .collect()
        }
    };
    println!("{:<8} {:<20} {:<44} STATE", "VMID", "ENTRY", "PATH");
    let mut ok = true;
    for vmid in vmids {
        let publication = entry::read_publication(&store, vmid)?;
        if only.is_none() && !publication.has_publish_key() {
            continue;
        }
        if let Some(why) = not_syncable(vmid)? {
            row(vmid, "-", "-", &why);
            continue;
        }
        match guest::inspect(&mut Pct { vmid }, &publication, false) {
            Ok(ins) => {
                if let Some(w) = &ins.warning {
                    row(vmid, "-", "-", &format!("warning: {w}"));
                }
                if ins.plan.items.is_empty() {
                    row(vmid, "-", "-", "nothing published");
                }
                for item in &ins.plan.items {
                    let path = item.path.as_ref().map(|p| p.as_str()).unwrap_or("-");
                    row(vmid, &item.entry, path, &item.action.to_string());
                }
            }
            Err(e) => {
                ok = false;
                row(vmid, "-", "-", &format!("error: {e:#}"));
            }
        }
    }
    Ok(ok)
}
