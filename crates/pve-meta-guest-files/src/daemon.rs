//! The loop, one per node, acting only on containers whose config lives on
//! this node. PVE keeps exactly one owner per guest, so a daemon on every
//! node never overlaps, and a migration moves the responsibility with the
//! guest.
//!
//! Every [`Timing::poll`] it reads the vmlist, which containers run (and the
//! inode of each one's command socket), and the digest of each local
//! container's document, and syncs a running container it watches when
//!
//! * it has not synced it yet, or its document's digest moved;
//! * it was not running at the last poll, or restarted since;
//! * [`Timing::drift`] has passed since its last sync, which is what catches a
//!   file edited or removed inside the guest; [`Timing::retry`] after a
//!   failed one.
//!
//! A container is watched while its document has a `guest-files` key, and after
//! that for as long as its manifest had records at its last sync, so removing
//! the key cleans up. A stopped container is skipped until it starts, a locked
//! one (a backup, a snapshot, a migration) until the lock is gone.
//!
//! **A read error is never an absent document.** When the vmlist, the running
//! containers or any local container's document cannot be read, the poll ends
//! there with nothing synced.
//!
//! Single-threaded on purpose: one container at a time. Logs go to stderr,
//! which is the journal under systemd. A write is logged every time it is
//! done, with why the sync ran; every state that holds (a local edit kept, a
//! refusal, a failing write or sync, a lock, the store being unavailable) once
//! when it begins.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::time::{Duration, Instant};

use anyhow::Result;
use pve_meta_core::store::{DocId, MetaStore};

use crate::entry::{self, GuestFiles};
use crate::guest::{self, Outcome};
use crate::lock::GuestLock;
use crate::node::{self, Kind};
use crate::pct::Pct;

/// How often the loop does what.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll: Duration,
    pub drift: Duration,
    pub retry: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            poll: Duration::from_secs(10),
            drift: Duration::from_secs(600),
            retry: Duration::from_secs(60),
        }
    }
}

/// Why a container is synced now, in the order they are served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    Changed,
    Started,
    Drift,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Reason::Changed => "document changed",
            Reason::Started => "container started",
            Reason::Drift => "drift check",
        })
    }
}

/// What the loop remembers of one container.
#[derive(Debug, Clone, Default)]
pub struct Memo {
    /// The document digest the last sync was of.
    pub digest: Option<String>,
    /// The command socket's inode at the last poll; `None` when stopped.
    pub inode: Option<u64>,
    /// Its manifest had records at the last sync.
    pub watched: bool,
    pub next_due: Option<Instant>,
}

/// Whether a container is synced at this poll, and why. `inode` is its
/// command socket's, `None` when it is not running.
pub fn due(
    memo: Option<&Memo>,
    inode: Option<u64>,
    digest: Option<&str>,
    has_guest_files_key: bool,
    now: Instant,
) -> Option<Reason> {
    inode?;
    if !has_guest_files_key && !memo.is_some_and(|m| m.watched) {
        return None;
    }
    let Some(m) = memo else {
        return Some(Reason::Changed);
    };
    if m.digest.as_deref() != digest {
        Some(Reason::Changed)
    } else if m.inode != inode {
        Some(Reason::Started)
    } else if m.next_due.is_none_or(|t| now >= t) {
        Some(Reason::Drift)
    } else {
        None
    }
}

/// Lines logged once per state: by guest (0 for the node) and key, the last one logged.
#[derive(Default)]
pub struct Log {
    seen: HashMap<(u32, String), String>,
}

impl Log {
    /// Logs `line` unless `key` last logged it; `None` ends the state; `true` if it changed.
    pub fn once(&mut self, vmid: u32, key: &str, line: Option<String>) -> bool {
        let k = (vmid, key.to_string());
        match line {
            Some(l) if self.seen.get(&k) != Some(&l) => {
                match vmid {
                    0 => eprintln!("{l}"),
                    v => eprintln!("{v}: {l}"),
                }
                self.seen.insert(k, l);
                true
            }
            Some(_) => false,
            None => self.seen.remove(&k).is_some(),
        }
    }

    /// Ends every state of `vmid` whose key starts with `prefix` and is not in
    /// `keep`.
    pub fn settle(&mut self, vmid: u32, prefix: &str, keep: &BTreeSet<String>) {
        self.seen.retain(|(v, k), _| *v != vmid || !k.starts_with(prefix) || keep.contains(k));
    }
}

/// The loop's whole state.
#[derive(Default)]
pub struct Daemon {
    pub memos: HashMap<u32, Memo>,
    wanted: HashMap<u32, (Option<String>, GuestFiles)>,
    pub log: Log,
}

impl Daemon {
    /// Polls forever.
    pub fn run(timing: Timing) -> Result<()> {
        let node = node::nodename()?;
        let store = entry::open_store();
        eprintln!(
            "pve-meta-guest-files on {node}: polling every {}s, drift check every {}s",
            timing.poll.as_secs(),
            timing.drift.as_secs()
        );
        eprintln!("warning: deprecated, removed in 0.4: pve-meta writes nothing into guests");
        let mut d = Daemon::default();
        loop {
            d.tick(&store, &node, timing, Instant::now());
            std::thread::sleep(timing.poll);
        }
    }

    fn unavailable(&mut self, why: String) {
        self.log.once(0, "unavailable", Some(format!("{why}; waiting")));
    }

    /// One poll. Anything that cannot be read, the vmlist, the running
    /// containers, a document, ends the poll with nothing done: a read error
    /// is never taken for an absent document.
    pub fn tick(&mut self, store: &MetaStore, node: &str, timing: Timing, now: Instant) {
        let (vmlist, active) = match (node::vmlist(), node::active_containers()) {
            (Ok(l), Ok(a)) => (l, a),
            (Err(e), _) | (_, Err(e)) => return self.unavailable(format!("{e:#}")),
        };
        let local: Vec<u32> = vmlist
            .iter()
            .filter(|(_, g)| g.node == node && g.kind == Kind::Lxc)
            .map(|(vmid, _)| *vmid)
            .collect();
        let mut next = HashMap::new();
        for &vmid in &local {
            let digest = match store.digest_of(&DocId::Guest(vmid)) {
                Ok(d) => d,
                Err(e) => return self.unavailable(format!("the document of {vmid}: {e}")),
            };
            let wanted = match self.wanted.remove(&vmid) {
                Some((d, p)) if d == digest => p,
                _ => match entry::read_guest_files(store, vmid) {
                    Ok(p) => p,
                    Err(e) => return self.unavailable(format!("{e:#}")),
                },
            };
            next.insert(vmid, (digest, wanted));
        }
        if self.log.once(0, "unavailable", None) {
            eprintln!("available again");
        }
        self.wanted = next;
        self.memos.retain(|vmid, _| local.contains(vmid));

        let mut queue = Vec::new();
        for (&vmid, (digest, wanted)) in &self.wanted {
            let inode = active.get(&vmid).copied();
            let memo = self.memos.get(&vmid);
            match due(memo, inode, digest.as_deref(), wanted.has_guest_files_key(), now) {
                Some(reason) => queue.push((reason, vmid)),
                None => {
                    if let Some(m) = self.memos.get_mut(&vmid) {
                        m.inode = inode;
                    }
                }
            }
        }
        queue.sort();
        for (reason, vmid) in queue {
            if self.sync(node, vmid, &active, reason, timing, now) {
                self.memos.entry(vmid).or_default().inode = active.get(&vmid).copied();
            }
        }
    }

    /// One sync of a due container; `false` when it was skipped and is to be
    /// tried again at the next poll.
    fn sync(
        &mut self,
        node: &str,
        vmid: u32,
        active: &BTreeMap<u32, u64>,
        reason: Reason,
        timing: Timing,
        now: Instant,
    ) -> bool {
        let status = node::stopped_or_locked(node, vmid, active).map_err(|e| format!("{e:#}"));
        let held = match &status {
            Err(e) => Some(e.clone()),
            Ok(Some(s)) => Some(format!("{s}; waiting")),
            Ok(None) => None,
        };
        let skip = held.is_some();
        self.log.once(vmid, "lock", held);
        if skip {
            return false;
        }
        // A hand-run sync holds it: the next poll tries again.
        let _lock = match GuestLock::take(vmid, false) {
            Ok(Some(l)) => l,
            Ok(None) => return false,
            Err(e) => {
                self.log.once(vmid, "lock", Some(format!("{e:#}")));
                return false;
            }
        };
        let (digest, wanted) = self.wanted[&vmid].clone();
        let result = guest::sync(&mut Pct { vmid }, &wanted, false);
        let memo = self.memos.entry(vmid).or_default();
        memo.digest = digest;
        let report = match result {
            Ok(r) => r,
            Err(e) => {
                memo.watched |= wanted.has_guest_files_key();
                memo.next_due = Some(now + timing.retry);
                self.log.once(vmid, "sync", Some(format!("sync failed: {e:#}")));
                return true;
            }
        };
        memo.watched = report.has_records;
        memo.next_due = Some(now + timing.drift);
        self.log.once(vmid, "sync", None);
        self.log.once(vmid, "warning", report.warning.clone());
        let mut states = BTreeSet::new();
        for d in &report.items {
            let Some(line) = guest::describe(d) else { continue };
            match &d.outcome {
                Some(Outcome::Done | Outcome::Changed) => eprintln!("{vmid} ({reason}): {line}"),
                _ => {
                    let key = format!("entry {} {:?}", d.item.entry, d.item.path);
                    self.log.once(vmid, &key, Some(line));
                    states.insert(key);
                }
            }
        }
        self.log.settle(vmid, "entry ", &states);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_detection() {
        let now = Instant::now();
        let later = now + Duration::from_secs(60);
        let memo = |digest: &str, inode: Option<u64>, watched: bool| Memo {
            digest: Some(digest.into()),
            inode,
            watched,
            next_due: Some(later),
        };
        let seen = memo("a", Some(7), false);

        assert_eq!(due(None, None, Some("a"), true, now), None);
        assert_eq!(due(None, Some(7), Some("a"), true, now), Some(Reason::Changed));
        // No key and nothing recorded: never looked into.
        assert_eq!(due(None, Some(7), Some("a"), false, now), None);
        assert_eq!(due(Some(&seen), Some(7), Some("b"), false, now), None);
        assert_eq!(due(Some(&seen), Some(7), Some("a"), true, now), None);
        assert_eq!(due(Some(&seen), Some(7), Some("b"), true, now), Some(Reason::Changed));
        assert_eq!(due(Some(&seen), None, Some("b"), true, now), None);
        assert_eq!(
            due(Some(&memo("a", None, false)), Some(7), Some("a"), true, now),
            Some(Reason::Started)
        );
        assert_eq!(due(Some(&seen), Some(8), Some("a"), true, now), Some(Reason::Started));
        assert_eq!(due(Some(&seen), Some(7), Some("a"), true, later), Some(Reason::Drift));
        // The key removed while records remain: still synced, so the files go.
        assert_eq!(
            due(Some(&memo("a", Some(7), true)), Some(7), None, false, now),
            Some(Reason::Changed)
        );
    }

    #[test]
    fn a_state_is_logged_once() {
        let mut log = Log::default();
        assert!(log.once(105, "entry t", Some("kept".into())));
        assert!(!log.once(105, "entry t", Some("kept".into())));
        assert!(log.once(105, "entry t", Some("refused".into())));
        log.settle(105, "entry ", &BTreeSet::new());
        assert!(log.once(105, "entry t", Some("refused".into())));
        assert!(log.once(0, "unavailable", Some("x".into())));
        assert!(log.once(0, "unavailable", None));
        assert!(!log.once(0, "unavailable", None));
    }
}
