# Guest lifecycle: one patched file

pve-meta carries guest metadata through five lifecycle events — create, destroy,
snapshot, rollback and delete-snapshot — all hooked in one file, `PVE/AbstractConfig.pm`.
Clone and backup are not carried, and nothing runs on a timer. This is deliberate
(`docs/DESIGN.md` §6, §10, §11) — a smaller, honestly documented guarantee beats a wide
one with a silent gap.

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
`pve-ext/README.md` ("Managed patches") for the tool itself. What each hook does is
`docs/DESIGN.md` §6; the exports themselves, with their arguments and return values,
are documented where they are defined, in `crates/pve-meta-perl/src/lib.rs`.

One caveat worth knowing, not fixing: vzdump's own transient `'vzdump'` snapshot for LXC
containers goes through this same `snapshot_create`/`snapshot_delete` code, so it fires
`on_snapshot` and `on_delsnap` too — a container backup produces a short-lived extra
snapshot copy of its metadata document that disappears again a few seconds later.
Harmless, but visible if you go looking. QEMU's vzdump path never creates an
`AbstractConfig`-level snapshot, so this is LXC-only.

## Create and destroy: hooks, not a GC

Both live in `PVE::AbstractConfig` — the file already patched for the snapshot trio — so
the whole guest lifecycle costs one patched file in one package.

`destroy_config` is two lines upstream (`unlink` the config, die if that fails) and is the
single choke point for **every** destroy path in both guest packages: primary destroy,
create/restore failure cleanup, clone failure cleanup, remote-migration abort. Twelve call
sites, one method. The hook runs *after* the unlink succeeds — while the config exists, so
does the guest.

`create_and_lock_config` is the matching choke point for creation (create, restore, clone
target, `qm`/`pct` CLI, both remote-migration inbound paths). It opens with
`PVE::Cluster::check_vmid_unused($vmid, $allow_existing)`, which is what makes the hook
precise rather than a guess: when `$allow_existing` is false, PVE has just asserted this
vmid was free, so anything still under `/etc/pve/meta/<vmid>.*` is a leftover and is
cleared. When it is true — a restore *over* an existing guest — the document is kept,
because a backup does not carry one and clearing would be data loss.

This replaces the hourly GC, and it is not merely faster. A sweep nominates vmids that are
missing from the vmlist; a guest destroyed and recreated at the same vmid between two
sweeps is therefore never stale, and the new guest inherits the old document **for good**.
The create hook closes that window instead of narrowing it, and covers the destroy that
never ran because its node was down.

For a config removed out of band, where `destroy_config` never ran, `pve-meta ls --orphans`
and `pve-meta rm <vmid>` are the manual cleanup; no timer sweeps.

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
