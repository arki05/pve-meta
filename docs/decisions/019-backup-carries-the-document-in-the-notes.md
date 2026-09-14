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
