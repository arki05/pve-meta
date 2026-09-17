# 008 — The guest lifecycle is one patched file; clone is not carried

**Status:** accepted; the backup half is superseded by 019.

## Context

Metadata should follow a guest through snapshot and rollback, and must not survive a
destroy or leak into a guest recreated at the same vmid. PVE has no hook for any of
this.

## Decision

One patched file, `PVE/AbstractConfig.pm` in `libpve-guest-common-perl` — the least
churned package with a seam — carries five hooks: `on_create` (only when PVE has just
asserted the vmid was unused), `on_destroy` (every destroy path in `pve-container` and
`qemu-server` funnels through it), and the snapshot trio, all `eval`-wrapped and warn:
metadata never breaks a guest operation. Managed by `pve-ext-patch` (dpkg-divert,
`--fuzz=0`, `perl -c` gate, re-applied by a trigger on reship). Clone is not carried:
patching two more files reshipped on every point release is not worth it for metadata a
hand copy through the API restores in one command.

## Consequences

Migration needs nothing: the document is cluster-wide, at a flat pmxcfs path visible
identically from every node.
