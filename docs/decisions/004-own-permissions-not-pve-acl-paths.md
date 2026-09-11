# 004 — Permissions are pve-meta's own files, not PVE ACL paths

**Status:** accepted; open to revisit.

## Context

The natural PVE way to scope a token to a prefix would be an ACL path such as
`/vms/<vmid>/meta/<prefix>`, granted with `pveum`, shown in the Permissions panel.
`PVE::AccessControl::check_path` is a hardcoded whitelist of paths, and `/meta/...` or a
deeper `/vms/<vmid>/...` is not in it: the API refuses such a path (verified live:
`400 invalid ACL path`). The whitelist is enforced in exactly one place, the ACL update
handler, while the `user.cfg` parser only normalizes, so a hand-written entry would load
and evaluate — a trap, not a supported opening.

## Decision

Permissions live in `meta.d/permissions/*.yaml`, with tag selectors, and Perl hands Rust
the PVE ACL answers plus the guest's tags; Rust computes the scopes.

## Alternative, recorded for when it is revisited

One regex hunk in `libpve-access-control` (applied by `pve-ext-patch`) would admit
`/vms/<vmid>/meta/<prefix>`, `/pool/<pool>/meta/<prefix>` and `/tag/<tag>/meta/<prefix>`.
`Meta.pm` already computes ACL answers itself, so it could ask `rpcenv` about those
paths per declared prefix and hand Rust the same scope list. The Rust core would not
change. The permission files, their parser, the Grants grid and Add Rule would go, and
`pveum` and the Permissions panel would be the whole administration surface. Costs: one
more diverted file, terse ACL entries instead of self-describing YAML, and the
requirement that a prefix be declared before it can be granted. Not done because it
adds a patch to a fourth package for a benefit that only matters once other people
administer the system.
