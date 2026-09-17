# 008 — The guest lifecycle is one patched file; clone and backup are not carried

**Status:** accepted; the backup half is superseded by [019](019-backup-carries-the-document-in-the-notes.md) (2026-09-14), which carries the document in the archive's copy of the guest's notes and patches one file each in `qemu-server` and `pve-container` for it.

## Context

Metadata should follow a guest through snapshot and rollback, and must not survive a
destroy or leak into a guest recreated at the same vmid. PVE has no hook for any of
this. The direction document asked for zero patched files.

## Decision

One patched file, `PVE/AbstractConfig.pm` in `libpve-guest-common-perl` — the least
churned package with a seam — carries five hooks: `on_create` from
`create_and_lock_config` (only when PVE has just asserted the vmid was unused),
`on_destroy` from `destroy_config` (which every destroy path in `pve-container` and
`qemu-server` funnels through), and the snapshot trio. All are `eval`-wrapped and warn;
metadata never breaks a guest operation. The patch is managed by `pve-ext-patch`
(dpkg-divert, `--fuzz=0`, `perl -c` gate, re-applied by a trigger on reship).

Clone and backup are not carried. Carrying clone would patch two more files reshipped
on every point release; a QEMU VM's backup has no room for a foreign blob, so a partial
guarantee would be worse than an honest one. Metadata lives in `/etc/pve`; back that up.

## Consequences

Migration needs nothing: the document is cluster-wide. The `ceilings.toml` watcher
exists because of this file: it verifies the patch still applies to each new upstream
release.
