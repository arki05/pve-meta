# Guest lifecycle: three patched files

pve-meta carries guest metadata through five lifecycle events — create, destroy,
snapshot, rollback and delete-snapshot — all hooked in one file, `PVE/AbstractConfig.pm`,
and through backup and restore, hooked in that same file and in the two vzdump plugins.
Clone is not carried, and nothing runs on a timer. This is deliberate (`docs/DESIGN.md`
§7; decisions 008, 009 and 019) — a smaller, honestly documented guarantee beats a wide
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

Every call runs under the document's own `cfs_lock_domain("pve-meta-<vmid>")`, the lock
every other writer takes, so a copy, a rollback and an in-flight API or CLI write never
interleave on the file — `rollback` in particular writes the document without a digest,
which nothing else would catch. All three run after the `lock_config` around the
snapshot operation has returned, so the document lock is taken on its own, once the
guest lock has been released -- unlike create and destroy, whose hooks take it inside
the guest lock; nothing that touches the document holds the guest lock at the time, so
there is no order between the two locks for these hooks to keep.

Every call is `eval { ... }; warn ... if $@;` — soft-fail, never blocks the actual
snapshot/rollback/delete-snapshot operation. A metadata read/write hiccup (disk full, a
corrupt document, the library not yet installed mid-upgrade) must never take down a
guest operation over a sidecar file, exactly as PVE already treats
`/etc/pve/firewall/*.fw` tolerantly in the same code paths.

How the diff reaches the installed file is not this document's subject: the manifest
(`patches/lifecycle.toml`) and the one diff
(`patches/lifecycle/libpve-guest-common-perl_AbstractConfig.pm.diff`) are applied by
`pve-ext-patch apply pve-meta-lifecycle` from `debian/pve-meta.postinst`, and
`pve-ext/README.md` ("Managed patches") owns the tool, its diversions and its limits. What each hook does is
`docs/DESIGN.md` §7; the exports themselves, with their arguments and return values,
are documented where they are defined, in `crates/pve-meta-perl/src/lib.rs`.

One caveat worth knowing, not fixing: vzdump's own transient `'vzdump'` snapshot for LXC
containers goes through this same `snapshot_create`/`snapshot_delete` code, so it fires
`on_snapshot` and `on_delsnap` too — a container backup produces a short-lived extra
snapshot copy of its metadata document that disappears again a few seconds later.
Harmless, but visible if you go looking. QEMU's vzdump path never creates an
`AbstractConfig`-level snapshot, so this is LXC-only.

## Create and destroy: hooks, not a GC

Both live in `PVE::AbstractConfig` — the file already patched for the snapshot trio — so
the whole guest lifecycle costs one patched file in one package; the backup side adds
one hook in each vzdump plugin (below).

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
cleared. When it is true — a restore *over* an existing guest, the only case that reaches
it — nothing is cleared here; the hook records the fact in a second node-local marker,
`/run/pve-meta/<vmid>.existing`, beside the restore marker, and the restore's own
`write_config` decides from the notes (below). A plain create removes that marker instead,
so a half-finished restore can never hand its mark to the next guest at that vmid.

This replaces the hourly GC, and it is not merely faster. A sweep nominates vmids that are
missing from the vmlist; a guest destroyed and recreated at the same vmid between two
sweeps is therefore never stale, and the new guest inherits the old document **for good**.
The create hook closes that window instead of narrowing it, and covers the destroy that
never ran because its node was down.

For a config removed out of band, where `destroy_config` never ran, `pve-meta ls --orphans`
and `pve-meta rm <vmid>` are the manual cleanup; no timer sweeps.

## Backup and restore: the notes block

A vzdump backup of either guest type carries the guest config, and a config's notes
carry arbitrary text. That is the whole mechanism (`docs/DESIGN.md` §7; decision 019):

* **Backup.** `assemble` in `PVE/VZDump/QemuServer.pm` (`qemu-server`) and in
  `PVE/VZDump/LXC.pm` (`pve-container`) calls `PVE::RS::Meta::export_for_backup($vmid)`
  and appends what it returns — the document, verbatim, between a `[pve-meta v1 …]`
  header and a `[/pve-meta]` end line, fenced for markdown — to the *archive's copy* of
  the notes: the `qemu-server.conf` vzdump writes into its tmpdir as one encoded `#`
  line per notes line, or the `description` of the fresh `$conf` the container plugin
  loads before it writes `pct.conf`. The live config never carries it. Both plugins
  write that copy before any archive path reads it, so the block rides through the QMP
  `backup` command's `config-file` for a VM with disks, the PBS client and `vma create`
  for a diskless one, the container tar and PBS paths, and the external-provider path,
  which hands the same text to the provider. A document above 16 KiB, or one that does
  not parse, is not carried; the backup log gets a warning line and the backup runs.
* **Restore on a host with pve-meta.** Every restore path of both guest types ends in
  `write_config`, in `PVE/AbstractConfig.pm`, the file already patched for the
  lifecycle. `write_config` cannot tell a restore from a `qm set`, but every restore
  (and create, and clone) begins with `create_and_lock_config`, in that same file, so
  that hook leaves a node-local marker for the vmid (`/run/pve-meta/<vmid>`). A create
  or restore keeps the config locked until it is done and writes it several times on
  the way — pve-container writes a bare `lock: create` skeleton mid-restore — so the
  marker is taken (one `unlink` on tmpfs) by the first `write_config` that carries a
  block, or by the first one of an unlocked config, and by nothing in between; only the
  write that took it imports. A create ends with an unlocked write. That closes the obvious hole: a block pasted into a live guest's notes by
  anyone with `VM.Config.Options` — the same privilege as full write on the document,
  but a path with no audit attribution — is never imported by an ordinary config
  write. When the marked write's notes carry the header it calls
  `PVE::RS::Meta::notes_import($vmid, $notes, 'restore')` under the document's
  `cfs_lock_domain` lock, inside the guest lock the caller holds — the same order
  create and destroy use — which writes the document through the store's own lint
  gate and hands back the notes without any block, and the config lands clean. Should
  the notes hold more than one, the **last** is imported: `assemble` appends its block
  after everything else, so an earlier one is never the backup's. The block wins over whatever document the vmid had: it is the
  backup being restored. Enforced schemas are not applied, since a restore is not an
  edit. An error (a block someone mangled, YAML the store refuses) warns and leaves the
  notes as they are, block included, for `pve-meta scan-notes`; nothing here can fail a
  restore. When the marked write carries **no** block and the `.existing` marker says the
  restore went over an existing guest, that guest's document and its snapshot copies are
  removed under the same lock: after a restore the guest is the backup, and a document the
  backup did not carry is the previous incarnation's — a stale ingress route or compose
  stack for something that no longer runs. A plain create takes this path too and finds
  nothing: `create_and_lock_config` cleared the vmid a moment earlier. Until that scan, a clone of such a guest copies the block and imports it on
  its own first write, since a clone begins with a create too.
* **Restore on a host without pve-meta.** The block stays in the notes. It renders on
  the Summary panel as a marked YAML code block, so the operator can see what the guest
  had, and it is deletable like any other notes text. QEMU's restore passes `#` lines
  verbatim and the container restore merges `description` whole, so nothing is lost or
  broken. Why plain text and not an encoding: PVE stores each notes line as one `#`
  line and percent-escapes every byte outside printable ASCII plus colon and percent,
  so the YAML round-trips byte for byte, and a blob would only take the readable
  fallback away. The one quirk of that format — a line beginning with `qmdump#` or
  `vzdump#` is dropped by vzdump — cannot arise from a document: the block's lines are
  a delimiter, top-level keys of the key charset, and indented lines.
* **Install.** `pve-meta scan-notes`, run once by `debian/pve-meta.postinst`, walks the
  cluster's vmlist and, for every guest whose notes carry the marker, calls
  `notes_import(..., 'install')`: a document already there was put there by a node
  that already runs pve-meta and is kept, the block is stripped either way. Cluster-wide,
  because `/etc/pve` is one filesystem: one install covers the cluster, and a later
  install on another node finds documents everywhere and blocks nowhere. The write goes
  to the config's own node path, not through `write_config`, which would write under the
  installing node's name; the guest lock is taken for this node's own guests only, since
  it is node-local and would serialise nothing for a guest another node owns.

The block's exact shape, the size cap and the parser live in
`crates/pve-meta-core/src/backup.rs`.

## What is not carried, and why

* **Clone.** A cloned guest's metadata is not copied to the new vmid. A clone without
  metadata is a minor inconvenience (copy it by hand through the API if you want it),
  not a correctness problem — carrying it would mean patching `pve-container`'s and
  `qemu-server`'s API2 clone handlers, two more files reshipped on every point release,
  for a "nice to have."
* **Migration** (same-cluster `qm`/`pct migrate`) needs no hook and isn't listed as a
  gap: `/etc/pve/meta/<vmid>.yaml` is a flat, cluster-wide pmxcfs path — like
  `/etc/pve/firewall/<vmid>.fw` and unlike the guest config itself — so it is visible
  identically from every node the instant a guest's ownership changes. Remote-migrate
  (cross-cluster) is out of scope for the same reason firewall config needs explicit
  transfer there and this doesn't attempt to match that.

Superseded design history (seven diffs across three packages, an `on_clone`/`on_destroy`
hook pair, a sidecar `meta.conf` blob that could not reach a VM with disks) is not
reproduced here; see `docs/decisions/008` and `019` for the reasoning and the
repository's git history if the old diffs themselves are ever needed for reference.
