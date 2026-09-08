# Guest lifecycle: the snapshot trio

pve-meta carries guest metadata through exactly three lifecycle events: snapshot,
rollback and delete-snapshot. Everything else is either handled by a GC job or not
carried at all. This is deliberate (`docs/DESIGN.md` §6, §10, §11) — a smaller, honestly
documented guarantee beats a wide one with a silent gap.

## The snapshot trio

One patched file, one package: `PVE/AbstractConfig.pm` (`libpve-guest-common-perl`),
shared by both `PVE::QemuConfig` and `PVE::LXC::Config` (neither subclass overrides the
three functions it touches). The diff:

* adds `use PVE::RS::Meta;` near the top of the file;
* in `snapshot_create`, after the snapshot has actually committed, calls
  `PVE::RS::Meta::on_snapshot($vmid, $snapname)`, which copies the guest's document to
  `/etc/pve/meta/<vmid>.<snapname>.yaml`;
* in `snapshot_delete`, after the snapshot entry is removed from the guest config, calls
  `PVE::RS::Meta::on_delsnap($vmid, $snapname)`, which removes that copy;
* in `snapshot_rollback`, after the rolled-back config is committed (the second,
  `$prepare = 0` pass — the first pass only stops the guest and sets `lock =>
  'rollback'`), calls `PVE::RS::Meta::on_rollback($vmid, $snapname)`, which restores the
  copy over the current document (or removes the current document if the guest had none
  at snapshot time).

Every call is `eval { ... }; warn ... if $@;` — soft-fail, never blocks the actual
snapshot/rollback/delete-snapshot operation. A metadata read/write hiccup (disk full, a
corrupt document, the library not yet installed mid-upgrade) must never take down a
guest operation over a sidecar file, exactly as PVE already treats
`/etc/pve/firewall/*.fw` tolerantly in the same code paths.

The patch, its manifest (`patches/lifecycle.toml`) and the one diff
(`patches/lifecycle/libpve-guest-common-perl_AbstractConfig.pm.diff`) are applied by
`pve-ext-patch apply pve-meta-lifecycle` from `debian/pve-meta.postinst` — see
`pve-ext/README.md` ("Managed patches") for the tool itself. Function contracts
(`on_snapshot`/`on_rollback`/`on_delsnap`) are specified in `docs/PERL-BINDINGS-SPEC.md`.

One caveat worth knowing, not fixing: vzdump's own transient `'vzdump'` snapshot for LXC
containers goes through this same `snapshot_create`/`snapshot_delete` code, so it fires
`on_snapshot` and `on_delsnap` too — a container backup produces a short-lived extra
snapshot copy of its metadata document that disappears again a few seconds later.
Harmless, but visible if you go looking. QEMU's vzdump path never creates an
`AbstractConfig`-level snapshot, so this is LXC-only.

## Destroy: GC, not a hook

There is no `on_destroy` hook. Instead, a systemd timer (`pve-meta-gc.timer`, hourly)
runs a GC job on every node that removes any guest document (and its snapshot copies)
whose vmid is no longer in the cluster's vmlist. See `README.md` for the unit and what
it runs.

This replaces the previous approach of patching `pve-container`'s and `qemu-server`'s
destroy paths directly: a GC is strictly simpler (one small script instead of five call
sites across two upstream packages, none of it reshipped on every point release) and
covers the same case — a destroyed guest's metadata eventually disappears — with a bound
of "up to one GC interval" instead of "immediately." There is no orphan concept in the
API: a document with no matching vmid is invisible to `/meta/guests` and 404s if you ask
for it directly, exactly as if it never existed; the GC just means it doesn't linger on
disk forever.

## What is not carried, and why

* **Clone.** A cloned guest's metadata is not copied to the new vmid. A clone without
  metadata is a minor inconvenience (copy it by hand through the API if you want it),
  not a correctness problem — carrying it would mean patching `pve-container`'s and
  `qemu-server`'s API2 clone handlers, two more files reshipped on every point release,
  for a "nice to have."
* **Backup and restore.** Metadata does not travel with a vzdump/PBS backup. The honest
  reason: a QEMU VM with at least one disk backs up through QMP's `backup` command,
  which has a fixed parameter set and cannot carry a third, arbitrary blob — there is no
  way to make this guarantee hold for every guest. An inconsistent guarantee ("works for
  containers, silently doesn't for disk-having VMs") is worse than none, because an
  administrator who trusts it gets bitten by exactly the case that doesn't work. The
  documented answer instead: metadata lives in `/etc/pve/meta`, which is part of
  `/etc/pve` — back that up like the rest of your cluster configuration.
* **Migration** (same-cluster `qm`/`pct migrate`) needs no hook and isn't listed as a
  gap: `/etc/pve/meta/<vmid>.yaml` is a flat, cluster-wide pmxcfs path — like
  `/etc/pve/firewall/<vmid>.fw` and unlike the guest config itself — so it is visible
  identically from every node the instant a guest's ownership changes. Remote-migrate
  (cross-cluster) is out of scope for the same reason firewall config needs explicit
  transfer there and this doesn't attempt to match that.

Superseded design history (seven diffs across three packages, an `on_clone`/`on_destroy`
hook pair, `export_for_backup`/`import_from_backup`) is not reproduced here; see
`docs/DESIGN.md` §11 for the one-line reasoning and the repository's git history if the
old diffs themselves are ever needed for reference.
