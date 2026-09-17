# 019 — A backup carries the document in the archive's copy of the guest's notes

**Status:** accepted. Supersedes the backup half of 008.

## Context

008 left backup out: a VM with disks backs up through QMP's fixed-parameter `backup`
command, so a sidecar blob could reach containers and diskless VMs only, and "back up
`/etc/pve`" is a cluster-level answer to a per-guest question. Rejected, one line each:
a sidecar file; a blob synced into the live notes (inverts the lock order); a vzdump
hook script (no restore-side seam); the firewall file (rewritten by any firewall edit).

## Decision

The document travels in the guest config, which every archive kind carries. `assemble`
in each vzdump plugin appends it to the archive's copy of the notes as one marked block,
plain YAML; the live config never carries it. `write_config`, which every restore path
ends in, reads the block into the store and strips it, winning over whatever document
the vmid had. A host without pve-meta restores it as readable notes text; `scan-notes`
imports it later, gated by a node-local marker so an ordinary write never imports one.

## Consequences

One form, a 16 KiB cap, held for every archive kind. Clone still does not carry the
document; one with an unimported block imports it on its own first write.
