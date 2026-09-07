# Lifecycle patch plan: wiring `PVE::RS::Meta` into PVE 9.2

**Status: §1-9 below are accurate research** — where each hook goes, why, and the full
diffs — and are implemented, live, and shipped: the seven diffs in §7 are, verbatim,
`patches/lifecycle/*.diff` in this repo, described by the manifest
`patches/lifecycle.toml`, applied by `pve-ext-patch apply pve-meta-lifecycle` from
`debian/pve-meta.postinst` (see `docs/DESIGN.md` §5 and §7, and `pve-ext/README.md`
for the tool). **§10 is SUPERSEDED** — it designs a bespoke `pve-meta-lifecycle-patch`
tool under a `pve-manager-patches/lifecycle/` layout that was never built; that need is
met instead by pve-ext's generic `pve-ext-patch` (see `pve-ext/README.md`, "Managed
patches"), left in place below only as a record of the design work that led there.

All quoted lines and line numbers below come from the actual installed Perl on a
disposable PVE 9.2.11 lab node (`pvemeta-node1`, reached as `root@10.10.10.154` via the
`arkantos` jump host), fetched read-only and diffed locally, at the time this research
was done.

Every insertion was validated with `perl -I<stub> -I/usr/share/perl5 -c <file>` on the
lab node against a throwaway stub `PVE::RS::Meta` (the real functions did not exist
yet at the time this research was done — they do now, see `crates/pve-meta-perl`).
All seven patched files report `syntax OK`; the only warnings seen (`Subroutine ... 
redefined`) are pre-existing and reproduce identically against the *unpatched* originals
(harmless `use base` / classic-Perl-plugin artifacts of running `perl -c` on a file
outside its normal `require` chain). Nothing was left behind on the lab node — the
`/tmp/check_*.pm` scratch copies and the stub module were removed after validation.

Function contracts (`on_snapshot`, `on_rollback`, `on_delsnap`, `on_clone`, `on_destroy`,
`export_for_backup`, `import_from_backup`) are as specified in
`docs/PERL-BINDINGS-SPEC.md` — this document only adds *where* and *how* to call them
from stock PVE Perl code, and *why* those exact spots.

## 1. Package/version table (tested ceiling)

All versions below are what is actually installed on the lab node right now
(`dpkg -l`), i.e. the newest version this research is valid against. Treat these as the
tested ceiling, not a floor — re-verify anchors on any newer release before relying on
the diffs unmodified.

| File | Package | Installed version | Arch |
|---|---|---|---|
| `PVE/AbstractConfig.pm` | `libpve-guest-common-perl` | 6.0.5 | all |
| `PVE/API2/LXC.pm`, `PVE/LXC/Create.pm`, `PVE/VZDump/LXC.pm`, `PVE/LXC/Config.pm` | `pve-container` | 6.1.14 | all |
| `PVE/API2/Qemu.pm`, `PVE/QemuServer.pm`, `PVE/VZDump/QemuServer.pm`, `PVE/QemuConfig.pm` | `qemu-server` | 9.2.7 | amd64 |
| (context only, not patched) | `pve-manager` | 9.2.11 | all |
| (context only, not patched) | `libpve-common-perl` | 9.2.1 | all |
| (context only, not patched) | `proxmox-backup-client` | 4.2.5-1 | amd64 |
| (precedent for the `PVE::RS::*` / perlmod pattern our new module follows) | `libpve-rs-perl` | 0.15.3 | amd64 |

`pveversion`: `pve-manager/9.2.11/f6997e698c7933ea (running kernel: 7.0.2-6-pve)`.

Relevant existing dependency edges (from `dpkg -s`), used below to reason about where
`use PVE::RS::Meta;` can go without inventing a new dependency direction PVE doesn't
already have:

```
libpve-guest-common-perl Depends: libpve-access-control, libpve-cluster-perl (>= 8.1.0),
  libpve-common-perl (>= 8.0.2), libpve-storage-perl (>= 8.3.4),
  proxmox-websocket-tunnel, pve-cluster, perl:any
  # NOTE: no pve-firewall, no libpve-rs-perl. PVE::Firewall is never used
  # from PVE::AbstractConfig.pm (confirmed: zero hits for "firewall" in the file).

pve-container Depends: ... libpve-guest-common-perl (>= 5.1.3), libpve-rs-perl (>= 0.11~),
  ... pve-firewall (>= 6.0.1), proxmox-backup-client (>= 3.2.3-1), ...

qemu-server Depends: ... libpve-guest-common-perl (>= 5.2.2), libpve-rs-perl (>= 0.14.0),
  ... pve-firewall (>= 6.0.3), ...
```

`pve-container` and `qemu-server` **already** depend on `libpve-rs-perl` (the existing
`PVE::RS::*` perlmod family — `PVE::RS::OCI`, `PVE::RS::TFA`, `PVE::RS::SDN`, etc., see
§9) and on `pve-firewall`. Adding a `Depends: libpve-meta-rs-perl` to those two packages
is precedented and unremarkable. `libpve-guest-common-perl` depends on neither
`pve-firewall` nor any `PVE::RS::*` package — see §8 for why the `AbstractConfig.pm`
edit is nonetheless the recommended location, with the trade-off called out explicitly.

## 2. Node execution summary (per verb)

`/etc/pve/meta/<vmid>.<ext>` is a **flat, cluster-wide path** — exactly like
`/etc/pve/firewall/<vmid>.fw` and unlike the guest config itself
(`/etc/pve/nodes/<node>/{qemu-server,lxc}/<vmid>.conf`, which *is* per-node and has a
real `move_config_to_node()` in `AbstractConfig.pm:285-293`). Evidence:
`PVE::Firewall::Helpers::remove_vmfw_conf`/`clone_vmfw_conf` (`Firewall/Helpers.pm:49-73`)
operate purely on `$pvefw_conf_dir/$vmid.fw` with no node parameter and no rename-to-node
logic anywhere in the firewall code. This means:

* **`on_snapshot` / `on_rollback` / `on_delsnap`** run wherever
  `AbstractConfig::snapshot_{create,delete,rollback}` run today — i.e. on the node that
  currently owns the guest (storage snapshot operations require local access to the
  volumes). No new node-targeting logic needed; same node as the existing call.
* **`on_clone`** runs on the node executing the clone worker, which is always the
  **source** node (`vzclone`/`qmclone` `fork_worker` calls in `API2/LXC.pm:2308` and
  `API2/Qemu.pm`), even when `$target` names a different node. When `$target` is set,
  only the *guest config* gets `rename()`-d to the target node's directory at the very
  end (`API2/LXC.pm:2300`: `PVE::LXC::Config->move_config_to_node($newid, $target)`).
  Because the meta file is flat/cluster-wide (not per-node), it needs **no equivalent
  move step** — it's already visible cluster-wide the instant `on_clone` writes it,
  precisely mirroring `clone_vmfw_conf`, which also does no node-aware work.
* **`on_destroy`** runs on the node handling the `vzdestroy`/clone-abort/restore-abort
  worker — always the node that currently owns (or was about to own) the vmid.
* **`export_for_backup` / `import_from_backup`** run on the node executing the vzdump
  backup job / the `pct create --restore` or `qm create --restore` worker — i.e.
  wherever vzdump/restore already runs today. No new node-targeting logic.
* **Migration** (`qm migrate` / `pct migrate`, same-cluster) needs **no hook at all**:
  the meta file lives in `/etc/pve/meta/`, which is on the cluster filesystem
  (`pmxcfs`), visible identically from every node the instant the guest config's
  ownership changes — there is nothing to move. This is stated in the task brief and is
  now confirmed by the flat-path evidence above, not merely assumed.
* **Remote-migrate** (`qm/pct remote-migrate`, cross-cluster, over the websocket
  mtunnel) is the one place firewall config *is* transferred explicitly, because two
  different `pmxcfs` instances are involved: `API2/LXC.pm:3228-3246` and
  `API2/Qemu.pm:6851-6873` receive a `'firewall-config'` string over the tunnel's
  `'config'` command and `PVE::Firewall::save_vmfw_conf()` it on the target; abort
  cleanup at `API2/LXC.pm:3364` / a symmetric spot in `API2/Qemu.pm` calls
  `remove_vmfw_conf($state->{vmid})`. **This is out of scope for the seven required
  verbs** (there is no `on_migrate` in the `PVE::RS::Meta` contract) and is **not**
  included in the diffs below — flagged here as a known gap for a future
  `on_remote_migrate_send`/`_receive` pair, analogous to `firewall-config`.

## 3. Per-verb plan

For every insertion the pattern is:

```perl
eval { PVE::RS::Meta::on_xxx(...) };
warn "pve-meta: on_xxx(...) failed: $@" if $@;
```

placed as its own statement (blank line before and after) immediately next to the
firewall-equivalent call (or, for snapshot/rollback/delsnap, at the one shared spot in
`AbstractConfig.pm` where both QEMU and LXC already funnel through identical code).
**All insertions use the soft eval+warn form; none use hard failure.** The argument is
the same at every site and is given once here rather than repeated verbatim seven times:

> `PVE::RS::Meta` is an optional structured-metadata sidecar
> (`README.md`: "knows nothing about operators") with no security semantics — unlike
> `/etc/pve/firewall/*.fw`, which PVE itself also treats tolerantly (`remove_vmfw_conf`
> silently no-ops via a bare `unlink` with no existence check or die-on-failure;
> `clone_vmfw_conf` unlinks-then-copies with no error handling at all — see
> `Firewall/Helpers.pm:49-73`). A metadata read/write hiccup (disk full, corrupt
> document, `libpve-meta-rs-perl` not yet installed on a rolling upgrade) must never
> block a snapshot, rollback, clone, destroy, or backup/restore of the actual guest.
> The one call (`import_from_backup`) that the spec documents as validating and dying
> on bad content is deliberately *not* promoted to a hard failure at the restore call
> site either: losing/skipping a metadata blob on restore is vastly preferable to
> failing an entire disk/container restore over a sidecar file, exactly as PVE already
> treats a firewall-config restore failure as skip-and-warn (see `$skip_fw`/`-e
> $pct_fwcfg_target` handling in `LXC/Create.pm`) rather than an abort.

### 3.1 `on_snapshot($vmid, $snapname)`

* **File**: `PVE/AbstractConfig.pm` (package `libpve-guest-common-perl`).
* **Function**: `snapshot_create` — shared by both `PVE::QemuConfig` and
  `PVE::LXC::Config` (neither subclass overrides it; see §8).
* **Anchor** (lines 869-879 of the installed file):

  ```perl
      if ($err) {
          warn "snapshot create failed: starting cleanup\n";
          eval { $class->snapshot_delete($vmid, $snapname, 1, $drivehash); };
          warn "$@" if $@;
          die "$err\n";
      }

      $class->__snapshot_commit($vmid, $snapname);
  }
  ```

* **Insertion**: right after `$class->__snapshot_commit($vmid, $snapname);` (line 878),
  before the closing `}` of `snapshot_create` — i.e. only once the snapshot has actually
  committed (the preceding `eval`/`die $err` block means we never reach line 878 on a
  failed snapshot, so this cannot fire for a snapshot that didn't really happen):

  ```perl
      $class->__snapshot_commit($vmid, $snapname);

      eval { PVE::RS::Meta::on_snapshot($vmid, $snapname) };
      warn "pve-meta: on_snapshot($vmid, $snapname) failed: $@" if $@;
  }
  ```

* **Caveat worth documenting, not fixing here**: vzdump's own transient `'vzdump'`
  snapshot for LXC (`VZDump/LXC.pm:238-251`,
  `PVE::LXC::Config->snapshot_create($vmid, 'vzdump', 0, "vzdump backup snapshot")`)
  goes through this exact same code path and will therefore also fire `on_snapshot`
  (and later `on_delsnap` when `VZDump/LXC.pm:565-597`'s `cleanup()` removes it a few
  seconds later). This is harmless — it just means the metadata document gets an extra
  short-lived snapshot copy named `vzdump` during every container backup — but is
  called out so it isn't mistaken for a bug when observed in the field. QEMU's vzdump
  path never creates an `AbstractConfig`-level snapshot (`VZDump/QemuServer.pm:1232-1235`,
  `sub snapshot { # nothing to do }`), so this only applies to containers.

### 3.2 `on_delsnap($vmid, $snapname)`

* **File**: `PVE/AbstractConfig.pm`.
* **Function**: `snapshot_delete`.
* **Anchor** (lines 1046-1055):

  ```perl
              delete $conf->{snapshots}->{$snapname};
              delete $conf->{lock};
              foreach my $volid (@$unused) {
                  $class->add_unused_volume($conf, $volid);
              }

              $class->write_config($vmid, $conf);
          },
      );
  }
  ```

* **Insertion**: after the closing `);` of the final `lock_config` call (line 1054),
  before the sub's closing `}` — this is the point at which the snapshot entry has
  actually been removed from the guest config on disk:

  ```perl
              $class->write_config($vmid, $conf);
          },
      );

      eval { PVE::RS::Meta::on_delsnap($vmid, $snapname) };
      warn "pve-meta: on_delsnap($vmid, $snapname) failed: $@" if $@;
  }
  ```

* Called from three places today, all correctly covered by this single edit: the
  normal `PVE::API2::{LXC,Qemu}::Snapshot` delete-snapshot API handler, the internal
  cleanup-on-failed-create branch inside `snapshot_create` itself (line 873:
  `eval { $class->snapshot_delete($vmid, $snapname, 1, $drivehash); };` — note `1` =
  `$force`, and `$drivehash` set means the earlier `$class->set_lock` / lock-acquisition
  in `snapshot_delete` is skipped, but the tail of the function, and thus our hook, is
  unchanged), and `VZDump/LXC.pm`'s post-backup `'vzdump'` snapshot cleanup (§3.1).

### 3.3 `on_rollback($vmid, $snapname)`

* **File**: `PVE/AbstractConfig.pm`.
* **Function**: `snapshot_rollback`.
* **Anchor** (lines 1204-1217; note the sub calls `$class->lock_config($vmid,
  $updatefn)` **twice** — once with `$prepare = 1` at line 1204, once with `$prepare =
  0` at line 1216 after the actual volume-level `__snapshot_rollback_vol_rollback`
  loop):

  ```perl
      $class->lock_config($vmid, $updatefn);

      $class->foreach_volume(
          $snap,
          sub {
              my ($vs, $volume) = @_;

              $class->__snapshot_rollback_vol_rollback($volume, $snapname);
          },
      );

      $prepare = 0;
      $class->lock_config($vmid, $updatefn);
  }
  ```

* **Insertion**: after the **second** `$class->lock_config($vmid, $updatefn);` (line
  1216 — the `$prepare = 0` / "commit" pass, which is what actually applies
  `__snapshot_apply_config` and writes the rolled-back config to disk), before the
  closing `}`:

  ```perl
      $prepare = 0;
      $class->lock_config($vmid, $updatefn);

      eval { PVE::RS::Meta::on_rollback($vmid, $snapname) };
      warn "pve-meta: on_rollback($vmid, $snapname) failed: $@" if $@;
  }
  ```

  Using the *second* occurrence (not the first, textually-identical-looking) call is
  deliberate and load-bearing: the first pass (`$prepare = 1`) only stops the guest and
  sets `lock => 'rollback'`; the guest config has not actually been replaced by the
  snapshot's config yet at that point (`__snapshot_apply_config` only runs when
  `!$prepare`, per the `if ($prepare) {...} else {...}` at lines 1178-1193). Hooking the
  first occurrence would fire `on_rollback` before the rollback happened.

### 3.4 `on_clone($vmid, $newid)`

* **Files**: `PVE/API2/LXC.pm` (`pve-container`), `PVE/API2/Qemu.pm` (`qemu-server`).
  `AbstractConfig.pm` has no clone concept at all — cloning is API2-layer-only in both
  stacks, exactly like firewall cloning (`PVE::Firewall::clone_vmfw_conf`), so this verb
  is naturally a per-stack, not a shared, edit.

**LXC** (`API2/LXC.pm:2059`, inside the `clone` API handler's `eval` block, right after
the guest's temporary `create` lock has been taken for `$newid` and right where the
firewall config is cloned forward *before* any disk copying begins):

```perl
            PVE::Firewall::clone_vmfw_conf($vmid, $newid);

            die "parameter 'storage' not allowed for linked clones\n"
                if defined($storage) && !$full;
```

Insertion:

```perl
            PVE::Firewall::clone_vmfw_conf($vmid, $newid);

            eval { PVE::RS::Meta::on_clone($vmid, $newid) };
            warn "pve-meta: on_clone($vmid, $newid) failed: $@" if $@;

            die "parameter 'storage' not allowed for linked clones\n"
                if defined($storage) && !$full;
```

**QEMU** (`API2/Qemu.pm:4617`, same shape, inside `clonefn`, right after the temporary
`# qmclone temporary file` config has been written for `$newid`):

```perl
            PVE::Firewall::clone_vmfw_conf($vmid, $newid);

            my $newvollist = [];
            my $jobs = {};
```

Insertion:

```perl
            PVE::Firewall::clone_vmfw_conf($vmid, $newid);

            eval { PVE::RS::Meta::on_clone($vmid, $newid) };
            warn "pve-meta: on_clone($vmid, $newid) failed: $@" if $@;

            my $newvollist = [];
            my $jobs = {};
```

* **Trade-off worth flagging explicitly** (per `docs/PERL-BINDINGS-SPEC.md`,
  `on_clone` "dies if the target document already exists"): calling it eagerly, right
  next to `clone_vmfw_conf`, means that if a stale `/etc/pve/meta/<newid>.<ext>` somehow
  already exists (e.g. a prior `on_destroy` failed silently, or `$newid` was reused
  faster than metadata cleanup caught up), `on_clone` will `die` inside the `eval`, we
  warn, and **the clone proceeds anyway with the old metadata left in place** attached
  to a semantically new guest. This is the one place where "soft-fail" has a real
  externally-visible downside (contaminated metadata) rather than just "feature
  didn't work this time". Two options considered:
  1. **(recommended, what's in the diff)** Keep eval+warn, matching `clone_vmfw_conf`'s
     own unconditional-overwrite tolerance (`Firewall/Helpers.pm:57-73`: it `unlink`s
     any existing target file with no die), and rely on `on_clone`'s die-on-conflict
     purely as a loud log signal that something upstream (probably `on_destroy` not
     being called, or being called and failing) needs investigating — not as a gate on
     the clone operation itself. This keeps clone's success/failure entirely about the
     guest, matching every other insertion in this document.
  2. Change `PVE::RS::Meta::on_clone` itself to overwrite (like firewall) instead of
     dying, eliminating the dilemma at the Perl call site entirely. That's a one-line
     change to the *Rust* module's semantics (not to any of these seven files) and
     would need to be reconciled with `docs/PERL-BINDINGS-SPEC.md`'s explicit
     "dies if the target document already exists" line — flagged here for whoever owns
     that spec to decide; this document does not change it.

### 3.5 `on_destroy($vmid)`

* **Files**: `PVE/API2/LXC.pm`, `PVE/API2/Qemu.pm`. Five call sites total, one per
  existing `remove_vmfw_conf` call in the *destroy* and *clone/restore-abort cleanup*
  paths (the two remote-migrate mtunnel abort-cleanup call sites, `API2/LXC.pm:3364`
  and its QEMU equivalent, are deliberately **not** patched — out of scope per §2).

**LXC, primary destroy** (`API2/LXC.pm:912-913`, the `vzdestroy` API handler, right
after the config's firewall rules are dropped and — per the existing comment two lines
above this block in the source — deliberately *before* `PVE::LXC::Config->destroy_config($vmid)`,
which is delayed "else we can have reuse race"; our call joins that same
"clean up everything else before freeing the vmid" group):

```perl
            PVE::AccessControl::remove_vm_access($vmid);
            PVE::Firewall::remove_vmfw_conf($vmid);
            if ($param->{purge}) {
```

Insertion:

```perl
            PVE::AccessControl::remove_vm_access($vmid);
            PVE::Firewall::remove_vmfw_conf($vmid);

            eval { PVE::RS::Meta::on_destroy($vmid) };
            warn "pve-meta: on_destroy($vmid) failed: $@" if $@;

            if ($param->{purge}) {
```

**LXC, restore/create-failure cleanup** (`API2/LXC.pm:626-634`, only taken when
`$destroy_config_on_error` — a fresh `pct create`/`pct restore` that failed partway and
must roll back the vmid it just allocated):

```perl
                if ($destroy_config_on_error) {
                    eval { PVE::LXC::Config->destroy_config($vmid) };
                    warn $@ if $@;

                    if (!$skip_fw_config_restore) { # Only if user has permission to change the fw
                        PVE::Firewall::remove_vmfw_conf($vmid);
                        warn $@ if $@;
                    }
                }
```

Insertion (joins the group unconditionally — unlike the firewall call, cleaning up our
own metadata does not need `$skip_fw_config_restore`'s permission gate, since we are
only ever removing a document `import_from_backup` may have just written moments ago
in this same failed attempt, not exposing/hiding pre-existing security-relevant state):

```perl
                if ($destroy_config_on_error) {
                    eval { PVE::LXC::Config->destroy_config($vmid) };
                    warn $@ if $@;

                    if (!$skip_fw_config_restore) { # Only if user has permission to change the fw
                        PVE::Firewall::remove_vmfw_conf($vmid);
                        warn $@ if $@;
                    }

                    eval { PVE::RS::Meta::on_destroy($vmid) };
                    warn "pve-meta: on_destroy($vmid) failed: $@" if $@;
                }
```

**LXC, clone-abort cleanup, two sites** (`API2/LXC.pm:2159-2167` — before any disk was
copied — and `API2/LXC.pm:2277-2285` — after some disks were copied; both destroy
`$newid`'s config and firewall rules on clone failure):

```perl
                    sub {
                        PVE::LXC::Config->destroy_config($newid);
                        PVE::Firewall::remove_vmfw_conf($newid);
                    },
```

and

```perl
                        sub {
                            my $conf = shift;
                            PVE::LXC::delete_ifaces_ipams_ips($conf, $newid);
                            PVE::LXC::Config->destroy_config($newid);
                            PVE::Firewall::remove_vmfw_conf($newid);
                        },
```

Insertion in both (shown for the first; the second gets the identical two lines added
after its `remove_vmfw_conf($newid);`):

```perl
                    sub {
                        PVE::LXC::Config->destroy_config($newid);
                        PVE::Firewall::remove_vmfw_conf($newid);

                        eval { PVE::RS::Meta::on_destroy($newid) };
                        warn "pve-meta: on_destroy($newid) failed: $@" if $@;
                    },
```

**QEMU, primary destroy** (`API2/Qemu.pm:2872-2873`, same shape as LXC):

```perl
                    PVE::AccessControl::remove_vm_access($vmid);
                    PVE::Firewall::remove_vmfw_conf($vmid);
                    if ($param->{purge}) {
```

Insertion: identical pattern to the LXC primary-destroy case above.

**QEMU, clone-abort cleanup** (`API2/Qemu.pm:4723`, one site — QEMU's clone error path
doesn't split into a before/after-disk-copy pair the way LXC's does):

```perl
                PVE::Firewall::remove_vmfw_conf($newid);

                unlink $conffile; # avoid races -> last thing before die
```

Insertion:

```perl
                PVE::Firewall::remove_vmfw_conf($newid);

                eval { PVE::RS::Meta::on_destroy($newid) };
                warn "pve-meta: on_destroy($newid) failed: $@" if $@;

                unlink $conffile; # avoid races -> last thing before die
```

### 3.6 `export_for_backup($vmid)`

Two independent stacks; the constraints differ sharply between them (see §4 for the
full PBS/vzdump findings this is based on).

**Container (`pve-container`)** — fully supported end-to-end, both PBS and local-tar:

* **File**: `PVE/VZDump/LXC.pm`, function `assemble()`
  (`VZDump/LXC.pm:325-361`), right where the firewall config is already staged into
  `$tmpdir/etc/vzdump/`:

  ```perl
      } else {
          if (-e $firewall) {
              PVE::Tools::file_copy($firewall, $fwconftmp);
          } else {
              PVE::Tools::file_set_contents($fwconftmp, '');
          }
          $task->{fw} = 1;
      }
  }
  ```

  Insertion (writes `pct.meta` next to `pct.fw`/`pct.conf`, using the exact string
  `export_for_backup` already returns per `docs/PERL-BINDINGS-SPEC.md`
  — `"#pve-meta-format: <ext>\n" + raw text` — so no extra header/wrapping is needed
  here):

  ```perl
      }

      my $meta = eval { PVE::RS::Meta::export_for_backup($vmid) };
      warn "pve-meta: export_for_backup($vmid) failed: $@" if $@;
      if (defined($meta)) {
          my $metaconftmp = "$tmpdir/etc/vzdump/pct.meta";
          PVE::Tools::file_set_contents($metaconftmp, $meta);
          $task->{meta} = 1;
      }
  }
  ```

* **File**: `PVE/VZDump/LXC.pm`, function `archive()`, three sub-paths, each gets one
  line mirroring the adjacent `fw`/`firewall` handling:
  * External backup-provider path (`archive():404-431`), next to
    `$info->{'firewall-config'}`: add `$info->{'meta-config'} =
    PVE::Tools::file_get_contents($meta_file) if -e $meta_file;` (provider-optional —
    see §4.3).
  * PBS path (`archive():447-453`), next to `push @$param, "fw.conf:$fw_conf";`:
    add `push @$param, "meta.conf:$meta_conf";` — this is a **plain
    `proxmox-backup-client backup <name>:<path> ...` CLI invocation** with an
    open-ended list of named blobs (see §4.1), so a third blob is trivial here, unlike
    the QEMU QMP path.
  * Local tar path (`archive():518-519`), next to `push @$tar, "./etc/vzdump/pct.fw"
    if $task->{fw};`: add `push @$tar, "./etc/vzdump/pct.meta" if $task->{meta};`.

* **Restore side** (`import_from_backup`) is the mirror image — see §3.7.

**QEMU (`qemu-server`)** — only partially supported; see §4 for the full reasoning.

* **File**: `PVE/VZDump/QemuServer.pm`, function `assemble()` (`:226-286`), right after
  the existing firewall copy:

  ```perl
      PVE::Tools::file_copy($firewall_src, $firewall_dest) if -f $firewall_src;
  }
  ```

  Insertion:

  ```perl
      PVE::Tools::file_copy($firewall_src, $firewall_dest) if -f $firewall_src;

      my $meta_dest = "$task->{tmpdir}/qemu-server.meta";
      my $meta = eval { PVE::RS::Meta::export_for_backup($vmid) };
      warn "pve-meta: export_for_backup($vmid) failed: $@" if $@;
      PVE::Tools::file_set_contents($meta_dest, $meta) if defined($meta);
  }
  ```

  This unconditionally stages `qemu-server.meta` in the task tmpdir — cheap, and
  harmless even on the paths below that currently cannot consume it, so a future
  QEMU-side fix (§4.2) needs no further Perl change here.

* **File**: `PVE/VZDump/QemuServer.pm`, `archive_pbs()` **diskless** branch
  (`:736-772`, `if (!$diskcount) { ... shells out to /usr/bin/proxmox-backup-client
  backup directly ... }`): add `push @$cmd, "meta.conf:$metaconf" if -e $metaconf;`
  next to the existing `push @$cmd, "fw.conf:$firewall" if -e $firewall;` — this path
  is a direct CLI call with the same open-ended blob list as the LXC PBS path, so it
  works today with no QEMU change.
* **File**: `PVE/VZDump/QemuServer.pm`, `archive_pbs()` **disk-having** branch
  (`:812-831`, the `mon_cmd($vmid, "backup", %$params)` QMP call) and `archive_vma()`'s
  equivalent QMP branch (`:1007-1017`): **no functional change is possible here** — a
  documentation-only comment is added at each site pointing at this file. See §4.2.
* **File**: `PVE/VZDump/QemuServer.pm`, `archive_vma()` **diskless** branch (`:945-947`,
  `['/usr/bin/vma', 'create', '-v', '-c', $conffile]`): add `push @$cmd, '-c', $metaconf
  if -e $metaconf;` — `vma create -c` is documented/used repeatedly (already twice) to
  accept multiple config blobs, so this is a genuine, working extra blob, not a
  workaround.
* **File**: `PVE/VZDump/QemuServer.pm`, `archive_external()` (backup-provider path,
  `:1511-1657`): add `$param->{'meta-config'} = PVE::Tools::file_get_contents($meta_file)
  if -e $meta_file;` next to `$param->{'firewall-config'}` — this is a plain Perl hash
  handed to the provider plugin, not QMP, so no format constraint applies here either
  (provider-optional, same caveat as LXC's backup-provider path, §4.3).

### 3.7 `import_from_backup($vmid, $string)`

**Container (`pve-container`)**, `PVE/LXC/Create.pm`, three restore paths, each mirrors
the file's existing firewall-restore block one-for-one:

* `restore_configuration_from_proxmox_backup` (`:448-483`), anchor:

  ```perl
      my $list = PVE::Storage::PBSPlugin::run_client_cmd($scfg, $storeid, "files", [$name]);
      my $has_fw_conf = grep { $_->{filename} eq 'fw.conf.blob' } @$list;

      if ($has_fw_conf) {
  ```

  Insertion (after the existing `if ($has_fw_conf) { ... }` block, still inside the
  function): a matching `my $has_meta_conf = grep { $_->{filename} eq
  'meta.conf.blob' } @$list;` check, fetching the blob via
  `PVE::Storage::PBSPlugin::run_raw_client_cmd(..., [$name, "meta.conf", "-"], outfunc
  => ...)` — the same "restore to stdout, capture in a string" idiom this very file
  already uses in `recover_config_from_proxmox_backup` (`:325-330`) for `pct.conf` —
  then `PVE::RS::Meta::import_from_backup($vmid, $meta_raw)`. Unlike the firewall
  block, this is **not** gated by `$skip_fw` (there is no metadata-specific permission
  concept in scope here; see the `on_destroy` restore-cleanup note in §3.5 for the same
  reasoning applied in the opposite direction).
* `restore_configuration_from_external_backup` (`:485-525`), anchor:

  ```perl
      my $firewall_config = $backup_provider->archive_get_firewall_config($volname, $storeid);

      if ($firewall_config) {
  ```

  Insertion after that `if` block: `if ($backup_provider->can('archive_get_meta_config'))
  { my $meta_config = $backup_provider->archive_get_meta_config($volname, $storeid); ...
  }` — guarded with `can()` since this is a *new* optional method on the
  `PVE::Storage::BackupProvider` plugin interface that no existing provider
  implements; providers that don't define it are silently skipped rather than dying
  with "Can't locate object method".
* `restore_configuration_from_etc_vzdump` (`:583-638`, the local-tar path), anchor:

  ```perl
              } else {
                  mkdir $pve_firewall_dir; # make sure the directory exists
                  PVE::Tools::file_copy($pct_fwcfg_fn, $pct_fwcfg_target);
              }
              unlink $pct_fwcfg_fn;
          }

      } elsif (-f $ovz_cfg_fn) {
  ```

  Insertion between `unlink $pct_fwcfg_fn;`'s closing `}` and the `} elsif`: a
  `my $pct_metacfg_fn = "$rootdir/etc/vzdump/pct.meta"; if (-f $pct_metacfg_fn && ...) {
  ...file_get_contents...; import_from_backup(...); unlink ... }` block matching the
  firewall block's existing three-guard style (`-f && !-l && -s`, i.e. reject symlinks
  and empty files — copied verbatim since it's a sane guard for any file recovered
  from an untrusted-ish extracted archive, not firewall-specific reasoning).

**QEMU (`qemu-server`)**, `PVE/QemuServer.pm`, three restore paths (restore is
*unconstrained* even where backup for disk-having VMs is not — see §4.2 — because
restore never goes through QMP; it's either a plain PBS "list files, fetch by name"
client call or a `vma extract` that dumps every embedded blob to `$tmpdir` regardless
of how many `-c` flags were used at backup time):

* `restore_proxmox_backup_archive` (`:6952-7051`), anchor:

  ```perl
          my $has_firewall_config =
              scalar(grep { $_->{filename} eq 'fw.conf.blob' } @{ $index->{files} });

          $param = [$pbs_backup_name, "qemu-server.conf", $cfgfn];
          PVE::Storage::PBSPlugin::run_raw_client_cmd($scfg, $storeid, $cmd, $param);

          if ($has_firewall_config) {
              $param = [$pbs_backup_name, "fw.conf", $firewall_config_fn];
              PVE::Storage::PBSPlugin::run_raw_client_cmd($scfg, $storeid, $cmd, $param);

              my $pve_firewall_dir = '/etc/pve/firewall';
              mkdir $pve_firewall_dir; # make sure the dir exists
              PVE::Tools::file_copy($firewall_config_fn, "${pve_firewall_dir}/$vmid.fw");
          }
  ```

  Insertion: a parallel `$has_meta_config` check plus an `if ($has_meta_config) { ...
  }` block fetching `"meta.conf"` to a tmp path and calling
  `PVE::RS::Meta::import_from_backup($vmid, $meta_raw)`. This will be a silent no-op
  today for every disk-having VM backup (nothing ever writes `meta.conf.blob` there —
  §4.2) but "just works" the moment either (a) a future `pve-qemu`/QMP change adds a
  third blob parameter, or (b) it's a backup of a **diskless** VM, which already can
  carry the blob via the direct-CLI path (§3.6).
* `restore_vma_archive`'s `$print_devmap` closure (`:7617-7631`), anchor:

  ```perl
          my $fwcfgfn = "$tmpdir/qemu-server.fw";
          if (-f $fwcfgfn) {
              my $pve_firewall_dir = '/etc/pve/firewall';
              mkdir $pve_firewall_dir; # make sure the dir exists
              PVE::Tools::file_copy($fwcfgfn, "${pve_firewall_dir}/$vmid.fw");
          }

          $virtdev_hash = $parse_backup_hints->($rpcenv, $user, $cfg, $fh, $devinfo, $opts);
  ```

  Insertion: a parallel `-f "$tmpdir/qemu-server.meta"` check calling
  `import_from_backup`. Same "works for diskless today, forward-compatible for
  disk-having VMs later" reasoning as above — `vma extract` (invoked a few lines
  earlier at `:7601`) has already unpacked *every* blob embedded at backup time into
  `$tmpdir`, unconditionally.
* `restore_external_archive` (`:7166-7235`), anchor:

  ```perl
          if ($data = $backup_provider->archive_get_firewall_config($volname)) {
              PVE::Tools::file_set_contents($firewall_config_fn, $data);
              my $pve_firewall_dir = '/etc/pve/firewall';
              mkdir $pve_firewall_dir; # make sure the dir exists
              PVE::Tools::file_copy($firewall_config_fn, "${pve_firewall_dir}/$vmid.fw");
          }

          my $fh = IO::File->new($cfgfn, "r") or die "unable to read qemu-server.conf - $!\n";
  ```

  Insertion: `can('archive_get_meta_config')`-guarded fetch + `import_from_backup`,
  same pattern as the LXC external-provider restore path.

## 4. vzdump / PBS findings (part a)

This is the crux of "can we carry a third blob beyond `config-file`/`firewall-file`":
**the answer depends entirely on which of four distinct backup code paths a given
backup takes**, and containers vs. VMs land on opposite sides of the QMP boundary.

### 4.1 Containers: no limitation at all

`pct` never talks QMP. Every container backup path — local tar (`tar cpf`, `VZDump/LXC.pm:497-562`),
PBS (a direct `proxmox-backup-client backup` CLI invocation, `VZDump/LXC.pm:445-494`),
and the external backup-provider path (a plain Perl hash, `VZDump/LXC.pm:404-444`) —
already accepts an open-ended list of named blobs/keys:

```perl
# VZDump/LXC.pm:447-453 (PBS path) — this list is not schema-limited at all:
my $param = [];
push @$param, "pct.conf:$tmpdir/etc/vzdump/pct.conf";
my $fw_conf = "$tmpdir/etc/vzdump/pct.fw";
if (-f $fw_conf) {
    push @$param, "fw.conf:$fw_conf";
}
```

Adding `"meta.conf:$meta_conf"` here (§3.6) is exactly as valid as `"fw.conf:..."` —
`proxmox-backup-client backup <archive-name>:<path> ...` takes any number of
`name:path` pairs. Restore is equally generic: `restore_configuration_from_proxmox_backup`
(`LXC/Create.pm:448-483`) just lists the PBS snapshot's files (`run_client_cmd(...,
"files", ...)`) and greps for a filename — `meta.conf.blob` is exactly as fetchable as
`fw.conf.blob`. **No limitation, no workaround needed for containers.**

### 4.2 QEMU VMs with disks: hard-limited by QEMU's own QMP `backup` command

For any VM that has at least one disk (`$diskcount` truthy — the overwhelming majority
of real VMs), **both** the PBS backup path and the local-storage VMA path funnel
through the *same* mechanism: QEMU's own internal backup job, started via the QMP
`backup` monitor command:

```perl
# VZDump/QemuServer.pm:812-831 (archive_pbs, disk-having branch)
my $params = {
    format => "pbs",
    'backup-file' => $repo,
    ...
    'config-file' => $conffile,
};
...
$params->{'firewall-file'} = $firewall if -e $firewall;
...
my $res = eval { mon_cmd($vmid, "backup", %$params) };
```

```perl
# VZDump/QemuServer.pm:1007-1017 (archive_vma, disk-having branch — same QMP command,
# used for the VMA format when there is at least one disk to stream)
my $params = {
    'backup-file' => "/dev/fdname/backup",
    speed => $speed,
    'config-file' => $conffile,
    devlist => $devlist,
};
$params->{'firewall-file'} = $firewall if -e $firewall;
...
$qmpclient->queue_cmd($qmp_peer, $backup_cb, 'backup', %$params);
```

`mon_cmd(..., "backup", %$params)` is a **QMP command** — its parameter set is defined
by QEMU's own QAPI schema (compiled into `pve-qemu-kvm`, not this Perl) and accepts
only the fixed keys QEMU's PBS/VMA backup driver implements. Today that is
`config-file` and `firewall-file` (embedded as extra named blobs inside the resulting
PBS snapshot / VMA stream by QEMU itself, not by this Perl code). Passing a third key
like `meta-file` would either be silently dropped or rejected with a QMP
"unexpected parameter" error, depending on QEMU's schema strictness — **this cannot be
fixed by any Perl-only change**; it requires a `pve-qemu-kvm` patch adding a third
optional blob parameter to the backup job implementation, which is out of scope for
this repo (a Rust/Perl metadata store) entirely.

**Only the diskless-VM branches bypass QMP** and shell out directly to the standalone
binaries, which — like the container case — accept arbitrary extra blobs:

```perl
# VZDump/QemuServer.pm:743-764 (archive_pbs, diskless branch)
my $cmd = ['/usr/bin/proxmox-backup-client', 'backup', ...];
push @$cmd, "qemu-server.conf:$conffile";
push @$cmd, "fw.conf:$firewall" if -e $firewall;
```

```perl
# VZDump/QemuServer.pm:945-947 (archive_vma, diskless branch)
my $cmd = ['/usr/bin/vma', 'create', '-v', '-c', $conffile];
push @$cmd, '-c', $firewall if -e $firewall;
```

`vma create -c` is used **twice already** in this exact snippet (once for the guest
config, once for firewall) — it is documented/implemented to accept a repeated `-c`
for an arbitrary number of named config blobs, so `push @$cmd, '-c', $metaconf if -e
$metaconf;` (§3.6) is a real, working third blob, not a hack. Diskless VMs are rare in
practice (a VM with zero drives), so this closes only a small slice of the gap, but it
is a genuine, zero-risk win taken in the diffs.

**Least-bad workaround for the disk-having-VM gap**, in descending order of
invasiveness:

1. **Do nothing beyond what's in this diff set.** `export_for_backup`/`import_from_backup`
   are still wired everywhere they *can* work (containers unconditionally; diskless
   VMs; the external-backup-provider path, which is pure Perl and unconstrained either
   way — §4.3); disk-having-VM-via-vzdump/PBS is the one gap, and it degrades
   gracefully to "no metadata in that one backup", never to a broken backup.
2. **`--notes-template`-style side channel**: vzdump's backup *notes* (the free-text
   annotation stored alongside the PBS snapshot / dumpdir file, expandable via
   `--notes-template` placeholders) could theoretically be abused to smuggle a small
   metadata payload, but notes are plain user-facing text with a small set of
   placeholders (`{{cluster}}`, `{{guestname}}`, `{{node}}`, `{{vmid}}`, `{{id}}`) —
   not an arbitrary-payload channel, and doing this would pollute a UI element humans
   actually read. **Not recommended.**
3. **Daemon-side archive (recommended if closing this gap matters)**: have the
   `pve-metad` daemon (or a vzdump hook script,
   `man vzdump` `--script`/`/etc/vzdump/vzdump.conf` style) independently archive
   `export_for_backup($vmid)`'s output next to (or inside a small tar alongside) the
   PBS backup — keyed by `$vmid` + the vzdump job's own timestamp/UPID so it can be
   correlated with a specific backup snapshot after the fact — rather than trying to
   embed it *inside* the PBS/VMA object itself. This sidesteps the QMP schema
   limitation entirely at the cost of the metadata snapshot no longer being physically
   inside the same PBS backup group (a separate retention/pruning story). A vzdump
   `job-hooks` script (PVE 8+) run in the `backup-end` phase already receives `$vmid`
   and the target volume ID and is the natural place to trigger this without touching
   any of the seven files in this document.
4. **Upstream `pve-qemu-kvm`/QEMU patch** adding a third QMP `backup` parameter (e.g.
   `meta-file`) mirroring `firewall-file` exactly, plus a symmetric addition to the vma
   tool / PBS restore index — the "correct" long-term fix, but a QEMU-side change,
   not something this Perl-only patch set can carry.

### 4.3 Backup-provider plugins: unconstrained, but need a new optional method

Both `archive_get_firewall_config` (restore) and `'firewall-config'` (backup) on the
external-backup-provider path are **plain Perl** — a hash key handed to
`$backup_provider->backup_{vm,container}(...)` on the way out, and a plain method call
on the way back in. Neither goes through QMP or a fixed wire format. Adding
`'meta-config'` / `archive_get_meta_config` is therefore free at the Perl-call-site
level (§3.6/§3.7) — the only real constraint is that **existing third-party backup
provider plugins don't implement `archive_get_meta_config` yet**, hence every restore
call site guards it with `$backup_provider->can('archive_get_meta_config')` rather than
calling it unconditionally (which would die with "Can't locate object method" against
every currently-shipping provider). This is a genuinely optional, additive extension
to `PVE::Storage::BackupProvider`'s plugin interface, not a hard requirement — a
provider that never implements it simply never gets metadata backed up/restored
through that path, with no error.

## 5. Files that need **no** changes, and why

* **`PVE/QemuConfig.pm`** and **`PVE/LXC/Config.pm`**: both subclass
  `PVE::AbstractConfig` and neither overrides `snapshot_create`, `snapshot_delete`,
  `snapshot_rollback`, or `destroy_config` — they only implement the `__snapshot_*`
  *internal* hooks (`__snapshot_save_vmstate`, `__snapshot_rollback_vol_rollback`,
  etc.) that the shared `AbstractConfig` methods call into for volume-level work.
  Patching `AbstractConfig.pm` once (§3.1-3.3) covers both stacks; there is nothing
  guest-type-specific about *when* a snapshot/rollback/delsnap commits.
* The remote-migrate mtunnel handlers in `API2/LXC.pm`/`API2/Qemu.pm` (§2) are
  consciously left unpatched — no verb in the `PVE::RS::Meta` contract covers them.

## 6. `use PVE::RS::Meta;` placement

| File | Placed after | Rationale |
|---|---|---|
| `PVE/AbstractConfig.pm` | `use PVE::Replication;` (last `use`, line 14) | Alphabetically last of the existing block; simplest possible diff. |
| `PVE/API2/LXC.pm` | `use PVE::RRD;` / before `use PVE::RS::OCI;` | File is (mostly) alphabetized case-sensitively; `RS::Meta` sorts before `RS::OCI`. |
| `PVE/API2/Qemu.pm` | `use PVE::Firewall;` / before `use PVE::API2::Firewall::VM;` | File is not alphabetized; grouped next to the other firewall-adjacent `use`, which is where every call site this file needs lives. |
| `PVE/LXC/Create.pm` | `use PVE::LXC::Setup;` / before `use PVE::VZDump::ConvertOVZ;` | Keeps the `PVE::LXC::*` cluster together; simple insertion point. |
| `PVE/VZDump/LXC.pm` | `use PVE::LXC;` / before `use PVE::Storage;` | Matches the file's loose-alphabetical existing order. |
| `PVE/VZDump/QemuServer.pm` | `use PVE::RESTEnvironment qw(log_warn);` / before `use PVE::QMPClient;` | File is not alphabetized; this is simply an uncontested gap next to another infra `use`. |
| `PVE/QemuServer.pm` | `use PVE::RPCEnvironment;` (before `use PVE::SafeSyslog;`) | Matches the file's loose-alphabetical existing order; keeps the diff minimal (one line, no reordering of surrounding `use`s). |

None of these are load-bearing choices — Perl doesn't care about `use` order across
independent modules — they were chosen only to keep each diff to the smallest,
least-surprising possible hunk.

## 7. Full unified diffs

Generated with `diff -u <orig> <patched> --label a/<path> --label b/<path>` from copies
fetched read-only from the lab node, hand-edited, and `perl -c`-validated on the node
against a throwaway stub `PVE::RS::Meta`. The same seven files are shipped in this repo
at `patches/lifecycle/`, described by `patches/lifecycle.toml`:

| Diff file (in `patches/lifecycle/`) | Target | Package |
|---|---|---|
| `libpve-guest-common-perl_AbstractConfig.pm.diff` | `PVE/AbstractConfig.pm` | libpve-guest-common-perl |
| `pve-container_API2-LXC.pm.diff` | `PVE/API2/LXC.pm` | pve-container |
| `pve-container_LXC-Create.pm.diff` | `PVE/LXC/Create.pm` | pve-container |
| `pve-container_VZDump-LXC.pm.diff` | `PVE/VZDump/LXC.pm` | pve-container |
| `qemu-server_API2-Qemu.pm.diff` | `PVE/API2/Qemu.pm` | qemu-server |
| `qemu-server_QemuServer.pm.diff` | `PVE/QemuServer.pm` | qemu-server |
| `qemu-server_VZDump-QemuServer.pm.diff` | `PVE/VZDump/QemuServer.pm` | qemu-server |

### `libpve-guest-common-perl_AbstractConfig.pm.diff`

```diff
--- a/PVE/AbstractConfig.pm
+++ b/PVE/AbstractConfig.pm
@@ -12,6 +12,7 @@
 use PVE::GuestHelpers qw(typesafe_ne);
 use PVE::ReplicationConfig;
 use PVE::Replication;
+use PVE::RS::Meta;
 
 my $nodename = PVE::INotify::nodename();
 
@@ -876,6 +877,9 @@
     }
 
     $class->__snapshot_commit($vmid, $snapname);
+
+    eval { PVE::RS::Meta::on_snapshot($vmid, $snapname) };
+    warn "pve-meta: on_snapshot($vmid, $snapname) failed: $@" if $@;
 }
 
 # Check if the snapshot might still be needed by a replication job.
@@ -1052,6 +1056,9 @@
             $class->write_config($vmid, $conf);
         },
     );
+
+    eval { PVE::RS::Meta::on_delsnap($vmid, $snapname) };
+    warn "pve-meta: on_delsnap($vmid, $snapname) failed: $@" if $@;
 }
 
 # Remove replication snapshots to make a rollback possible.
@@ -1214,6 +1221,9 @@
 
     $prepare = 0;
     $class->lock_config($vmid, $updatefn);
+
+    eval { PVE::RS::Meta::on_rollback($vmid, $snapname) };
+    warn "pve-meta: on_rollback($vmid, $snapname) failed: $@" if $@;
 }
 
 # Calculate a derived property from a configuration. Derived properties are:
```

### `pve-container_API2-LXC.pm.diff`

```diff
--- a/PVE/API2/LXC.pm
+++ b/PVE/API2/LXC.pm
@@ -19,6 +19,7 @@
 use PVE::RESTHandler;
 use PVE::RPCEnvironment;
 use PVE::RRD;
+use PVE::RS::Meta;
 use PVE::RS::OCI;
 use PVE::ReplicationConfig;
 use PVE::SSHInfo;
@@ -631,6 +632,9 @@
                         PVE::Firewall::remove_vmfw_conf($vmid);
                         warn $@ if $@;
                     }
+
+                    eval { PVE::RS::Meta::on_destroy($vmid) };
+                    warn "pve-meta: on_destroy($vmid) failed: $@" if $@;
                 }
                 die "$emsg $err";
             }
@@ -911,6 +915,10 @@
 
             PVE::AccessControl::remove_vm_access($vmid);
             PVE::Firewall::remove_vmfw_conf($vmid);
+
+            eval { PVE::RS::Meta::on_destroy($vmid) };
+            warn "pve-meta: on_destroy($vmid) failed: $@" if $@;
+
             if ($param->{purge}) {
                 print "purging CT $vmid from related configurations..\n";
                 PVE::ReplicationConfig::remove_vmid_jobs($vmid);
@@ -2058,6 +2066,9 @@
 
             PVE::Firewall::clone_vmfw_conf($vmid, $newid);
 
+            eval { PVE::RS::Meta::on_clone($vmid, $newid) };
+            warn "pve-meta: on_clone($vmid, $newid) failed: $@" if $@;
+
             die "parameter 'storage' not allowed for linked clones\n"
                 if defined($storage) && !$full;
 
@@ -2159,6 +2170,9 @@
                     sub {
                         PVE::LXC::Config->destroy_config($newid);
                         PVE::Firewall::remove_vmfw_conf($newid);
+
+                        eval { PVE::RS::Meta::on_destroy($newid) };
+                        warn "pve-meta: on_destroy($newid) failed: $@" if $@;
                     },
                 );
             };
@@ -2278,6 +2292,9 @@
                             PVE::LXC::delete_ifaces_ipams_ips($conf, $newid);
                             PVE::LXC::Config->destroy_config($newid);
                             PVE::Firewall::remove_vmfw_conf($newid);
+
+                            eval { PVE::RS::Meta::on_destroy($newid) };
+                            warn "pve-meta: on_destroy($newid) failed: $@" if $@;
                         },
                     );
                 };
```

### `pve-container_LXC-Create.pm.diff`

```diff
--- a/PVE/LXC/Create.pm
+++ b/PVE/LXC/Create.pm
@@ -17,6 +17,7 @@
 use PVE::DataCenterConfig;
 use PVE::LXC;
 use PVE::LXC::Setup;
+use PVE::RS::Meta;
 use PVE::VZDump::ConvertOVZ;
 use PVE::Tools;
 use POSIX;
@@ -480,6 +481,20 @@
             PVE::Storage::PBSPlugin::run_raw_client_cmd($scfg, $storeid, $cmd, $param);
         }
     }
+
+    my $has_meta_conf = grep { $_->{filename} eq 'meta.conf.blob' } @$list;
+    if ($has_meta_conf) {
+        my $meta_raw = '';
+        my $meta_outfunc = sub { my $line = shift; $meta_raw .= "$line\n"; };
+        my $meta_param = [$name, "meta.conf", "-"];
+        PVE::Storage::PBSPlugin::run_raw_client_cmd(
+            $scfg, $storeid, "restore", $meta_param,
+            outfunc => $meta_outfunc,
+        );
+
+        eval { PVE::RS::Meta::import_from_backup($vmid, $meta_raw) };
+        warn "pve-meta: import_from_backup($vmid) failed: $@" if $@;
+    }
 }
 
 sub restore_configuration_from_external_backup {
@@ -521,6 +536,15 @@
         }
     }
 
+    # optional: only providers that implement this method can supply pve-meta data
+    if ($backup_provider->can('archive_get_meta_config')) {
+        my $meta_config = $backup_provider->archive_get_meta_config($volname, $storeid);
+        if ($meta_config) {
+            eval { PVE::RS::Meta::import_from_backup($vmid, $meta_config) };
+            warn "pve-meta: import_from_backup($vmid) failed: $@" if $@;
+        }
+    }
+
     return;
 }
 
@@ -615,6 +639,14 @@
             unlink $pct_fwcfg_fn;
         }
 
+        my $pct_metacfg_fn = "$rootdir/etc/vzdump/pct.meta";
+        if (-f $pct_metacfg_fn && !-l $pct_metacfg_fn && -s $pct_metacfg_fn) {
+            my $meta_raw = PVE::Tools::file_get_contents($pct_metacfg_fn);
+            eval { PVE::RS::Meta::import_from_backup($vmid, $meta_raw) };
+            warn "pve-meta: import_from_backup($vmid) failed: $@" if $@;
+            unlink $pct_metacfg_fn;
+        }
+
     } elsif (-f $ovz_cfg_fn) {
         print "###########################################################\n";
         print "Converting OpenVZ configuration to LXC.\n";
```

### `pve-container_VZDump-LXC.pm.diff`

```diff
--- a/PVE/VZDump/LXC.pm
+++ b/PVE/VZDump/LXC.pm
@@ -13,6 +13,7 @@
 use PVE::LXC::Config;
 use PVE::LXC::Namespaces;
 use PVE::LXC;
+use PVE::RS::Meta;
 use PVE::Storage;
 use PVE::Tools;
 use PVE::VZDump;
@@ -358,6 +359,14 @@
         }
         $task->{fw} = 1;
     }
+
+    my $meta = eval { PVE::RS::Meta::export_for_backup($vmid) };
+    warn "pve-meta: export_for_backup($vmid) failed: $@" if $@;
+    if (defined($meta)) {
+        my $metaconftmp = "$tmpdir/etc/vzdump/pct.meta";
+        PVE::Tools::file_set_contents($metaconftmp, $meta);
+        $task->{meta} = 1;
+    }
 }
 
 sub archive {
@@ -428,6 +437,9 @@
         };
         $info->{'firewall-config'} = PVE::Tools::file_get_contents($firewall_file)
             if -e $firewall_file;
+        my $meta_file = "$tmpdir/etc/vzdump/pct.meta";
+        $info->{'meta-config'} = PVE::Tools::file_get_contents($meta_file)
+            if -e $meta_file;
         $info->{'bandwidth-limit'} = $opts->{bwlimit} * 1024 if $opts->{bwlimit};
 
         $backup_provider->backup_container_prepare($vmid, $info);
@@ -452,6 +464,11 @@
             push @$param, "fw.conf:$fw_conf";
         }
 
+        my $meta_conf = "$tmpdir/etc/vzdump/pct.meta";
+        if (-f $meta_conf) {
+            push @$param, "meta.conf:$meta_conf";
+        }
+
         my $rootdir = $snapdir;
         push @$param, "root.pxar:$rootdir";
 
@@ -517,6 +534,7 @@
         # the second parameter gives the structure in the tar.
         push @$tar, "--directory=$tmpdir", './etc/vzdump/pct.conf';
         push @$tar, "./etc/vzdump/pct.fw" if $task->{fw};
+        push @$tar, "./etc/vzdump/pct.meta" if $task->{meta};
         push @$tar, "--directory=$snapdir";
 
         my @findexcl_no_anchored = ();
```

### `qemu-server_API2-Qemu.pm.diff`

```diff
--- a/PVE/API2/Qemu.pm
+++ b/PVE/API2/Qemu.pm
@@ -56,6 +56,7 @@
 use PVE::INotify;
 use PVE::Network;
 use PVE::Firewall;
+use PVE::RS::Meta;
 use PVE::API2::Firewall::VM;
 use PVE::API2::Qemu::Agent;
 use PVE::API2::Qemu::HMPPerms;
@@ -2871,6 +2872,10 @@
 
                     PVE::AccessControl::remove_vm_access($vmid);
                     PVE::Firewall::remove_vmfw_conf($vmid);
+
+                    eval { PVE::RS::Meta::on_destroy($vmid) };
+                    warn "pve-meta: on_destroy($vmid) failed: $@" if $@;
+
                     if ($param->{purge}) {
                         print "purging VM $vmid from related configurations..\n";
                         PVE::ReplicationConfig::remove_vmid_jobs($vmid);
@@ -4616,6 +4621,9 @@
 
             PVE::Firewall::clone_vmfw_conf($vmid, $newid);
 
+            eval { PVE::RS::Meta::on_clone($vmid, $newid) };
+            warn "pve-meta: on_clone($vmid, $newid) failed: $@" if $@;
+
             my $newvollist = [];
             my $jobs = {};
 
@@ -4722,6 +4730,9 @@
 
                 PVE::Firewall::remove_vmfw_conf($newid);
 
+                eval { PVE::RS::Meta::on_destroy($newid) };
+                warn "pve-meta: on_destroy($newid) failed: $@" if $@;
+
                 unlink $conffile; # avoid races -> last thing before die
 
                 die "clone failed: $err";
```

### `qemu-server_QemuServer.pm.diff`

```diff
--- a/PVE/QemuServer.pm
+++ b/PVE/QemuServer.pm
@@ -42,6 +42,7 @@
 use PVE::PBSClient;
 use PVE::RESTEnvironment qw(log_warn);
 use PVE::RPCEnvironment;
+use PVE::RS::Meta;
 use PVE::SafeSyslog;
 use PVE::Storage;
 use PVE::SysFSTools;
@@ -7028,6 +7029,8 @@
         }
         my $has_firewall_config =
             scalar(grep { $_->{filename} eq 'fw.conf.blob' } @{ $index->{files} });
+        my $has_meta_config =
+            scalar(grep { $_->{filename} eq 'meta.conf.blob' } @{ $index->{files} });
 
         $param = [$pbs_backup_name, "qemu-server.conf", $cfgfn];
         PVE::Storage::PBSPlugin::run_raw_client_cmd($scfg, $storeid, $cmd, $param);
@@ -7041,6 +7044,16 @@
             PVE::Tools::file_copy($firewall_config_fn, "${pve_firewall_dir}/$vmid.fw");
         }
 
+        if ($has_meta_config) {
+            my $meta_config_fn = "$tmpdir/meta.conf";
+            $param = [$pbs_backup_name, "meta.conf", $meta_config_fn];
+            PVE::Storage::PBSPlugin::run_raw_client_cmd($scfg, $storeid, $cmd, $param);
+
+            my $meta_raw = PVE::Tools::file_get_contents($meta_config_fn);
+            eval { PVE::RS::Meta::import_from_backup($vmid, $meta_raw) };
+            warn "pve-meta: import_from_backup($vmid) failed: $@" if $@;
+        }
+
         my $fh = IO::File->new($cfgfn, "r")
             || die "unable to read qemu-server.conf - $!\n";
 
@@ -7219,6 +7232,13 @@
             PVE::Tools::file_copy($firewall_config_fn, "${pve_firewall_dir}/$vmid.fw");
         }
 
+        # optional: only providers that implement this method can supply pve-meta data
+        if ($backup_provider->can('archive_get_meta_config')
+            && ($data = $backup_provider->archive_get_meta_config($volname))) {
+            eval { PVE::RS::Meta::import_from_backup($vmid, $data) };
+            warn "pve-meta: import_from_backup($vmid) failed: $@" if $@;
+        }
+
         my $fh = IO::File->new($cfgfn, "r") or die "unable to read qemu-server.conf - $!\n";
 
         $virtdev_hash =
@@ -7628,6 +7648,13 @@
             PVE::Tools::file_copy($fwcfgfn, "${pve_firewall_dir}/$vmid.fw");
         }
 
+        my $metacfgfn = "$tmpdir/qemu-server.meta";
+        if (-f $metacfgfn) {
+            my $meta_raw = PVE::Tools::file_get_contents($metacfgfn);
+            eval { PVE::RS::Meta::import_from_backup($vmid, $meta_raw) };
+            warn "pve-meta: import_from_backup($vmid) failed: $@" if $@;
+        }
+
         $virtdev_hash = $parse_backup_hints->($rpcenv, $user, $cfg, $fh, $devinfo, $opts);
 
         foreach my $info (values %{$virtdev_hash}) {
```

### `qemu-server_VZDump-QemuServer.pm.diff`

```diff
--- a/PVE/VZDump/QemuServer.pm
+++ b/PVE/VZDump/QemuServer.pm
@@ -19,6 +19,7 @@
 use PVE::JSONSchema;
 use PVE::PBSClient;
 use PVE::RESTEnvironment qw(log_warn);
+use PVE::RS::Meta;
 use PVE::QMPClient;
 use PVE::Storage::Plugin;
 use PVE::Storage::PBSPlugin;
@@ -283,6 +284,11 @@
     }
 
     PVE::Tools::file_copy($firewall_src, $firewall_dest) if -f $firewall_src;
+
+    my $meta_dest = "$task->{tmpdir}/qemu-server.meta";
+    my $meta = eval { PVE::RS::Meta::export_for_backup($vmid) };
+    warn "pve-meta: export_for_backup($vmid) failed: $@" if $@;
+    PVE::Tools::file_set_contents($meta_dest, $meta) if defined($meta);
 }
 
 sub archive {
@@ -721,6 +727,7 @@
 
     my $conffile = "$task->{tmpdir}/qemu-server.conf";
     my $firewall = "$task->{tmpdir}/qemu-server.fw";
+    my $metaconf = "$task->{tmpdir}/qemu-server.meta";
 
     my $opts = $self->{vzdump}->{opts};
     my $scfg = $opts->{scfg};
@@ -762,6 +769,7 @@
 
         push @$cmd, "qemu-server.conf:$conffile";
         push @$cmd, "fw.conf:$firewall" if -e $firewall;
+        push @$cmd, "meta.conf:$metaconf" if -e $metaconf;
 
         $self->loginfo("starting diskless backup");
         $self->loginfo(join(' ', @$cmd));
@@ -829,6 +837,11 @@
 
         $params->{fingerprint} = $fingerprint if defined($fingerprint);
         $params->{'firewall-file'} = $firewall if -e $firewall;
+        # NOTE(pve-meta): the QMP 'backup' command only accepts the fixed set of extra
+        # blob parameters implemented by QEMU's PBS backup driver (currently 'config-file'
+        # and 'firewall-file'); a third 'meta-file' cannot be added here without a
+        # pve-qemu-side change. See docs/LIFECYCLE-PATCHES.md for the tracked limitation
+        # and workaround.
 
         $params->{encrypt} = defined($keyfile) ? JSON::true : JSON::false;
         if (defined($keyfile)) {
@@ -917,6 +930,7 @@
 
     my $conffile = "$task->{tmpdir}/qemu-server.conf";
     my $firewall = "$task->{tmpdir}/qemu-server.fw";
+    my $metaconf = "$task->{tmpdir}/qemu-server.meta";
 
     my $opts = $self->{vzdump}->{opts};
 
@@ -944,6 +958,7 @@
 
         my $cmd = ['/usr/bin/vma', 'create', '-v', '-c', $conffile];
         push @$cmd, '-c', $firewall if -e $firewall;
+        push @$cmd, '-c', $metaconf if -e $metaconf;
         push @$cmd, $outcmd;
 
         $self->loginfo("starting diskless backup");
@@ -1011,6 +1026,8 @@
                 devlist => $devlist,
             };
             $params->{'firewall-file'} = $firewall if -e $firewall;
+            # NOTE(pve-meta): same QMP 'backup' limitation as in archive_pbs() above - no
+            # arbitrary third blob parameter is available for disk-having VMs.
             $params->{fleecing} = JSON::true if $task->{'use-fleecing'};
             add_backup_performance_options($params, $opts->{performance}, $qemu_support);
 
@@ -1510,6 +1527,7 @@
 
     my $guest_config = PVE::Tools::file_get_contents("$task->{tmpdir}/qemu-server.conf");
     my $firewall_file = "$task->{tmpdir}/qemu-server.fw";
+    my $meta_file = "$task->{tmpdir}/qemu-server.meta";
 
     my $opts = $self->{vzdump}->{opts};
 
@@ -1655,6 +1673,8 @@
         $param->{'bandwidth-limit'} = $opts->{bwlimit} * 1024 if $opts->{bwlimit};
         $param->{'firewall-config'} = PVE::Tools::file_get_contents($firewall_file)
             if -e $firewall_file;
+        $param->{'meta-config'} = PVE::Tools::file_get_contents($meta_file)
+            if -e $meta_file;
 
         $backup_provider->backup_vm($vmid, $guest_config, $volumes, $param);
     };
```

## 8. The `AbstractConfig.pm` dependency-direction trade-off

`libpve-guest-common-perl` currently depends on neither `pve-firewall` nor any
`PVE::RS::*` package (§1) — this looks like a deliberate boundary: the lowest common
base class for guest configs doesn't know about the firewall subsystem at all, and
`PVE::Firewall` calls are made only from the API2 layer (`pve-container`/`qemu-server`),
which already depend on `pve-firewall`. Adding `use PVE::RS::Meta;` directly to
`AbstractConfig.pm` (§3.1-3.3) breaks that symmetry by making the base package depend
on a new leaf package.

Two ways to resolve this, both viable:

* **(A) — what's in the diff above.** Add `use PVE::RS::Meta;` to `AbstractConfig.pm`
  directly, add `libpve-meta-rs-perl` as a new `Depends:` of `libpve-guest-common-perl`.
  Simplest, one shared edit point instead of three, and `libpve-guest-common-perl`
  already depends on plenty (`PVE::Storage`, `PVE::ReplicationConfig`,
  `PVE::Replication` — it is not a dependency-minimal leaf package today), so this is
  arguably in character rather than a new precedent.
* **(B) — preserves the existing boundary.** Leave `AbstractConfig.pm` untouched, and
  instead override `snapshot_create`/`snapshot_delete`/`snapshot_rollback` in both
  `PVE::QemuConfig` and `PVE::LXC::Config` as thin wrappers (`sub snapshot_create { my
  ($class, @args) = @_; my $r = $class->SUPER::snapshot_create(@args); eval {
  PVE::RS::Meta::on_snapshot(...) }; ...; return $r; }`), with `use PVE::RS::Meta;`
  added to `PVE/QemuConfig.pm` and `PVE/LXC/Config.pm` instead — both already sit in
  packages (`qemu-server`, `pve-container`) that depend on `libpve-rs-perl`, so this
  adds no new dependency-direction precedent at all. Costs: three edit points instead
  of one, and the wrapper needs to preserve `wantarray`/return-value semantics of the
  wrapped call exactly (none of the three currently return anything meaningful, so this
  is low-risk but must be re-checked against the installed code before use).

This document ships (A) because it's what the task's own file list points at and is the
smaller diff, but (B) is a legitimate, equally-correct alternative if preserving
`libpve-guest-common-perl`'s current dependency footprint is a hard project constraint.

## 9. Precedent for the `PVE::RS::*` naming/packaging pattern

`libpve-rs-perl` (installed version `0.15.3` on the lab node) already ships exactly this
shape of module — a Rust cdylib exposed to Perl via `perlmod`, namespaced under
`PVE::RS::*`:

```
/usr/share/perl5/PVE/RS/Firewall/SDN.pm
/usr/share/perl5/PVE/RS/NVML.pm
/usr/share/perl5/PVE/RS/OCI.pm
/usr/share/perl5/PVE/RS/OpenId.pm
/usr/share/perl5/PVE/RS/ResourceScheduling/Dynamic.pm
/usr/share/perl5/PVE/RS/ResourceScheduling/Static.pm
/usr/share/perl5/PVE/RS/SDN/Fabrics.pm
/usr/share/perl5/PVE/RS/SDN/PrefixLists.pm
/usr/share/perl5/PVE/RS/SDN/RouteMaps.pm
/usr/share/perl5/PVE/RS/SDN/WireGuard/PrivateKeys.pm
/usr/share/perl5/PVE/RS/SDN.pm
/usr/share/perl5/PVE/RS/TFA.pm
/usr/share/perl5/Proxmox/Lib/PVE.pm
```

`PVE::RS::Meta` (shipped as `libpve-meta-rs-perl`, its own binary package rather than
folded into `libpve-rs-perl`, per `docs/PERL-BINDINGS-SPEC.md`) is a new sibling in this
already-established family, not a novel pattern for PVE to accept.

## 10. `pve-meta-patch` (lifecycle variant) tool design — SUPERSEDED

**This section is a historical record of the design work, kept for context; it was
never built as its own tool.** The need it identifies — apply the seven diffs from §7,
`dpkg-divert`-based, `perl -c`-gated, with `apply`/`remove`/`verify`/`status` — is met
instead by pve-ext's generic `pve-ext-patch`, applying `patches/lifecycle.toml` (see
`docs/DESIGN.md` §5 and `pve-ext/README.md`, "Managed patches"). Several of the
specific mechanics this section calls for (a per-path `dpkg-divert`, `patch -p1
--dry-run` before a real `patch -p1`, `perl -c` reporting `syntax OK`) are exactly what
`pve-ext-patch` does — generalized to an arbitrary manifest of files across an
arbitrary number of packages, not hardcoded to these seven.

`pve-manager-patch/pve-meta-patch` (an early, single-purpose prototype for the UI
template patch, since folded into pve-ext's `pve-manager.toml` manifest) established
the pattern this section proposed reusing for the seven Perl files above: **`dpkg-divert` the original away, so
the pristine file is always recoverable and automatically stays in sync with future
package upgrades; regenerate the "real" filename from the pristine copy plus our
insertions; validate before installing; provide `apply`/`remove`/`verify`/`status`.**
The lifecycle variant differs in three ways the existing tool doesn't need to handle:

1. **Seven files across three packages**, not one file in one package — needs one
   `dpkg-divert` per file (all owned by the same `pve-meta` diversion-package name, same
   as the existing tool), and per-file trigger paths.
2. **Multiple non-contiguous insertion points per file** (up to six per file, see §3),
   not "insert exactly one line after one anchor" — the existing tool's
   `render_patched_template`/`validate_rendered_template` `awk`-based single-anchor
   approach doesn't generalize cleanly. Recommended mechanism: ship the `.diff` files
   from §7 verbatim (already produced, already in `patches/lifecycle/` — see the
   note at the top of this section) and
   apply them with **`patch`** (context-based, so it tolerates unrelated nearby changes
   in a point release and *fails loudly* — non-zero exit, no `.rej` silently left behind
   uninspected — if an anchor genuinely moved), rather than re-implementing each
   insertion as bespoke `awk`/`sed`. `patch -p1 --dry-run` first (mirrors the existing
   tool's "validate before installing" philosophy), then a real `patch -p1` into a
   scratch copy of the pristine diverted file, never in place.
3. **Perl-specific acceptance gate**: after `patch` succeeds, run `perl -I
   /usr/share/perl5 -c <scratch-patched-file>` and require `syntax OK` before installing
   — this is a strictly stronger check than the existing tool's `verify` (which is
   grep-only, appropriate for HTML/JS but not for Perl where a misapplied hunk can
   produce a file that "greps fine" but doesn't compile).

### Files & layout (as proposed here; see the note at the top of this section for
### what was actually built)

```
pve-manager-patches/lifecycle/
  <package>_<file-basename>.diff   # the 7 diffs from §7, verbatim
  pve-meta-lifecycle-patch          # the new bash tool (this section)
  debian/
    pve-meta.triggers.lifecycle     # appended into the real pve-meta.triggers
    postinst.lifecycle              # appended into the real postinst
    postrm.lifecycle                # appended into the real postrm
```

(What was actually built: `patches/lifecycle/*.diff` + `patches/lifecycle.toml`,
applied by the generic `pve-ext-patch`, triggered via `debian/pve-meta.triggers` and
called from `debian/pve-meta.postinst`/`prerm` — no bespoke tool or per-manifest
`debian/*.lifecycle` fragments needed.)

### Per-file table the tool operates over

| Real path | Diverted-to | Owning package | Diff |
|---|---|---|---|
| `/usr/share/perl5/PVE/AbstractConfig.pm` | `...AbstractConfig.pm.pve-meta-orig` | libpve-guest-common-perl | `libpve-guest-common-perl_AbstractConfig.pm.diff` |
| `/usr/share/perl5/PVE/API2/LXC.pm` | `...LXC.pm.pve-meta-orig` | pve-container | `pve-container_API2-LXC.pm.diff` |
| `/usr/share/perl5/PVE/LXC/Create.pm` | `...Create.pm.pve-meta-orig` | pve-container | `pve-container_LXC-Create.pm.diff` |
| `/usr/share/perl5/PVE/VZDump/LXC.pm` | `...LXC.pm.pve-meta-orig` | pve-container | `pve-container_VZDump-LXC.pm.diff` |
| `/usr/share/perl5/PVE/API2/Qemu.pm` | `...Qemu.pm.pve-meta-orig` | qemu-server | `qemu-server_API2-Qemu.pm.diff` |
| `/usr/share/perl5/PVE/QemuServer.pm` | `...QemuServer.pm.pve-meta-orig` | qemu-server | `qemu-server_QemuServer.pm.diff` |
| `/usr/share/perl5/PVE/VZDump/QemuServer.pm` | `...QemuServer.pm.pve-meta-orig` | qemu-server | `qemu-server_VZDump-QemuServer.pm.diff` |

(Two pairs of files share a diverted-basename collision risk — `API2/LXC.pm` vs.
`API2/Qemu.pm` both end in a directory named `API2`, and both `VZDump/LXC.pm` and
`VZDump/QemuServer.pm` live under `VZDump/` — so the diversion target must use the
**full relative path** with `/` replaced consistently, e.g.
`/usr/share/perl5/PVE/API2/LXC.pm.pve-meta-orig` sitting right next to the real file in
the same directory, exactly as the existing tool does for `index.html.tpl` — not a
flattened basename registry.)

### Subcommands

```
pve-meta-lifecycle-patch apply [file...]     # divert (if needed) + patch + perl -c + install, all 7 (or a subset)
pve-meta-lifecycle-patch remove [file...]    # restore pristine + remove diversion, all 7 (or a subset)
pve-meta-lifecycle-patch verify [file...]    # patch --dry-run against the pristine copy; report per-file hunk status
pve-meta-lifecycle-patch status [file...]    # per-file: diverted? patched (grep for "PVE::RS::Meta")? in sync with the shipped .diff?
```

`apply` per file:

1. `require_cmd dpkg-divert patch perl`.
2. If not yet diverted: `dpkg-divert --package pve-meta --add --rename --divert
   <path>.pve-meta-orig <path>`. If diverted by another package: refuse (matches the
   existing tool's exact wording/behavior).
3. `cp <path>.pve-meta-orig <tmp>`; `patch -p1 --dry-run -d <tmpdir> < <diff>` first; on
   failure, abort **that file only** (leave its diversion alone if it already existed,
   remove it if `apply` just created it moments ago) and continue to the next file —
   one file's anchors moving must not block applying the other six.
4. On dry-run success: `patch -p1 -d <tmpdir> < <diff>` for real, into the scratch copy.
5. `perl -I/usr/share/perl5 -c <scratch-patched-file>` — must print `... syntax OK` on
   its last line (grep for the literal suffix, same idiom as the existing tool's
   anchor-grep checks). A pre-existing `Subroutine ... redefined` warning on the
   original file (§ intro) is expected and must not be treated as failure — only
   compare the final `syntax OK`/`had compilation errors` verdict, and only trust it
   if it appears on the **last** line of `perl -c`'s output (Perl always prints the
   verdict last).
6. `install -m 0644 <scratch-patched-file> <path>` (same-permissions install as the
   existing tool).
7. `cmd_status` for that file.

`remove` per file mirrors the existing tool's `remove` exactly (delete the live file,
`dpkg-divert --remove --rename`, fall back to `cp -a` from the pristine backup if
`dpkg-divert` itself fails) — no new logic needed here, since "restore the pristine
file" doesn't care how many hunks were applied.

`verify` runs step 3 above (dry-run only) against whatever pristine copy is available
(the diverted backup if already applied, else the live file) and reports, per file: `n`
of the diff's hunks that would apply cleanly vs. fail — this is strictly more precise
than a hand-written anchor-string list (the existing tool's `ANCHOR_CLASSES` array
tailored to one JS file) because the `.diff` context lines *are* the anchors, generated
directly from real installed source rather than re-derived by hand.

### Packaging glue (extends the existing `pve-manager-patch/debian/` files, doesn't replace them)

* **Triggers** — add one `interest-noawait` line per real path (not per package,
  since `dpkg-divert`'s trigger-on-nominal-path mechanism, already explained in
  `pve-manager-patch/README.md` §"Packaging proposal", applies identically here
  regardless of which of the three packages ships that path):

  ```
  interest-noawait /usr/share/perl5/PVE/AbstractConfig.pm
  interest-noawait /usr/share/perl5/PVE/API2/LXC.pm
  interest-noawait /usr/share/perl5/PVE/LXC/Create.pm
  interest-noawait /usr/share/perl5/PVE/VZDump/LXC.pm
  interest-noawait /usr/share/perl5/PVE/API2/Qemu.pm
  interest-noawait /usr/share/perl5/PVE/QemuServer.pm
  interest-noawait /usr/share/perl5/PVE/VZDump/QemuServer.pm
  ```

* **`postinst`**: on `configure`/`triggered`, run `pve-meta-lifecycle-patch apply`
  (all seven, best-effort per-file as above), never failing the package install if a
  patch application fails — identical philosophy to the existing tool's `postinst`
  (`|| { echo "...warning...apply failed..." >&2; }`), since a metadata lifecycle hook
  silently not being wired for one file on one PVE point release is far less bad than
  breaking `apt upgrade` for the whole host.
* **`postrm`**: on `remove`/`purge`, run `pve-meta-lifecycle-patch remove` (all seven),
  same tolerant-of-failure wrapping as the existing tool's `postrm`.
* **Backups**: before any `apply`, the existing `dpkg-divert --add --rename` step
  already guarantees a pristine backup exists (this *is* the backup mechanism — no
  separate `.orig`/`.bak` scheme needed, exactly as the UI patch tool already relies on
  for `index.html.tpl`). No additional backup step is required.
* **Re-apply via dpkg triggers on `interest-noawait`**: already covered by the trigger
  list above — `interest-noawait` (not `interest-await`) is deliberate and matches the
  existing tool's choice: PVE's own package upgrades (`pve-container`, `qemu-server`,
  `libpve-guest-common-perl`) should not have to wait for `pve-meta`'s postinst to run
  before dpkg considers *their* unpack complete; the re-patch happens asynchronously
  right after, same trade-off already accepted and documented for the UI patch.

### Why `patch`-based, not a rewrite of every insertion as bespoke shell logic

The existing tool's single-anchor `awk` approach is the right tool for "insert exactly
one literal line after exactly one literal needle, once, in one file." This job is
categorically different — up to six insertions per file, several inside near-identical-
looking sibling blocks (§3.4's two clone-abort cleanup sites in `API2/LXC.pm` are
textually almost identical and must not be conflated) — which is precisely the problem
context-diff hunks (three lines of context before/after, per hunk, as already generated
in §7) solve robustly and `grep`/`awk` needle-matching does not. Re-implementing each
hunk as a hand-written `awk` insertion would both be more code and be *more* fragile
(no context verification beyond the single needle line), not less.
