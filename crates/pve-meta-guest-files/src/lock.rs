//! One lock per guest on the node, so the daemon and a hand-run `sync` of the
//! same container take turns. A sync is a read, a decision and a write
//! several `pct` calls apart; two of them interleaved could each decide on
//! what the other is about to change.

use std::fs::{self, File};
use std::os::unix::io::AsRawFd;

use anyhow::{bail, Context, Result};

pub const DIR: &str = "/run/lock/pve-meta-guest-files";

pub struct GuestLock {
    _file: File,
}

impl GuestLock {
    /// Takes the lock, waiting for another holder when `wait`; `Ok(None)`
    /// when it is held and `wait` is not set.
    pub fn take(vmid: u32, wait: bool) -> Result<Option<GuestLock>> {
        fs::create_dir_all(DIR).with_context(|| format!("cannot create {DIR}"))?;
        let path = format!("{DIR}/{vmid}.lock");
        let file = File::create(&path).with_context(|| format!("cannot open {path}"))?;
        let op = if wait { libc::LOCK_EX } else { libc::LOCK_EX | libc::LOCK_NB };
        // SAFETY: a valid, open descriptor owned by `file` for the call.
        if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
            return Ok(Some(GuestLock { _file: file }));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) && !wait {
            return Ok(None);
        }
        bail!("flock {path}: {err}")
    }
}
