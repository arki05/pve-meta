# 019 — A backup carries the document in the archive's copy of the guest's notes

**Status:** accepted (2026-09-14). Supersedes the "backup is not carried" half of 008.

## Context

008 left backup out: a VM with at least one disk backs up through the QMP `backup`
command, whose parameter set is fixed at `config-file` and `firewall-file`, so a
sidecar blob could reach containers and diskless VMs only, and an inconsistent guarantee
was judged worse than none. The documented answer was to back up `/etc/pve`. That is a
cluster-level answer to a per-guest question: restore VM 105 from three weeks ago and
you get its disks and config from then, and its metadata from now, or not at all.

Five ways were weighed. A sidecar file or blob (008's own seven-diff design): no slot for
a VM with disks, new archive readers per format on the restore side, and on a stock tar
restore the file lands inside the container's filesystem. A blob in the live notes,
synced on every metadata write: zero patches, but it inverts the lock order against the
destroy hook, fails whenever the guest is locked, bumps the config digest under every
other API client, caps metadata by the user's own notes length, and clone copies notes,
so an adopt on create would inherit the source's document into the clone — the failure
009 closed. A blob in the backup volume's notes via the vzdump hook script: one global
script slot, notes that are editable and shown in the backup grid, and no restore-side
seam at all. The firewall file as a carrier: restored verbatim everywhere, but rewritten
without comments by any firewall edit. And the one below.

## Decision

The document travels in the guest config, which every archive kind carries. At backup
time `assemble` in each vzdump plugin appends it to the archive's copy of the notes as
one marked block, plain YAML, no encoding. The live config never carries it. At restore
time `write_config` — which every restore path of both guest types ends in, and which
lives in the file already patched for the lifecycle — reads the block into the store and
strips it before the config lands; the block wins over whatever document the vmid had.
A host without pve-meta restores the block as readable notes text. `pve-meta scan-notes`,
run once by the package install, does the same for every guest in the cluster whose
notes carry a block, keeping a document that is already there.

Two more patched files, one in `qemu-server` and one in `pve-container`, each one hook in
a small, stable function. Backup is a bigger gap than clone was, and the cost 008 refused
for clone is paid here for it.

## Consequences

Backup and restore hold for VMs with disks, containers, PBS, vma, tar and external
providers alike, with one form and one size cap (16 KiB of YAML; above it the backup log
says the document was not carried). `ceilings.toml` and the watcher track four packages.
A restore over an existing guest now replaces its document from the backup. Clone is
still not carried, and the live config still never carries metadata, so nothing about
snapshots, migration or the API changes.

Things this costs or leaves open, on purpose:

* **The notes are a way into the store, so the automatic import is gated.** Editing a
  guest's notes needs `VM.Config.Options`, which is already full write on the document
  (§5), so a pasted block grants nothing new; what it would lose is attribution, since
  the block's header names a vmid and a time its author chose and no authid. So
  `write_config` imports only while the node-local marker `create_and_lock_config` left
  is there — a restore, a create, a clone — and the marker is taken by the first write
  that carries a block or the first write of an unlocked config, since a create keeps
  the config locked and writes it more than once. An ordinary config write never
  imports. `pve-meta scan-notes` is the explicit, root-only way in for anything
  else. Signing the block with a cluster key was considered and not done: it would not
  close the one residual below, and a principal who can restore a guest can already
  craft its whole config.
* **The residual: a clone of a guest whose notes still carry a block.** A block stays
  in the live notes only when a restore's import failed, or after a restore on a host
  without pve-meta that no scan has visited yet. In that window a clone copies the notes
  and imports the block on its own first write, because a clone begins with a create. A
  scan closes the window.
* **A backup taken before pve-meta carries no block, and a restore from it over an
  existing guest keeps that guest's current document.** That is "metadata from now",
  the outcome this record's context calls wrong, and it is accepted for the one case
  where the backup has no opinion. Writing an empty block into every backup of a guest
  without a document would fix it at the price of a block in every restored guest's
  notes on stock hosts.
* **On a stock host, a block above the notes limit blocks notes editing until it is
  deleted.** Both guest schemas cap the notes at 8 KiB on API writes; restore does not
  check, so a larger block lands and renders, but the Notes editor refuses to save until
  the block is removed. The cap stays at 16 KiB because on a pve-meta host the block is
  stripped before any limit applies, and the cap should not penalise that path to spare
  the fallback. A non-ASCII document also grows in the config file, up to threefold per
  escaped byte, which pmxcfs does not mind.
* **The install scan writes other nodes' configs.** `/etc/pve` is one filesystem and
  the write is the whole-file replace pmxcfs makes atomic, but the guest lock is
  node-local, so a write on the owning node in the same instant could lose one side. The
  owning node's next scan or restore imports the same text again. Accepted for a
  one-time pass over the few guests that carry a block.
