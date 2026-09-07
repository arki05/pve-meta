# pve-meta-lifecycle-patch: wiring `PVE::RS::Meta` into stock PVE Perl

Applies the seven unified diffs derived in `docs/LIFECYCLE-PATCHES.md` (section 10 in
particular documents this tool's design) so that snapshot/rollback/delsnap/clone/destroy
and vzdump backup/restore call into `PVE::RS::Meta`'s `on_snapshot`, `on_rollback`,
`on_delsnap`, `on_clone`, `on_destroy`, `export_for_backup`, and `import_from_backup`
hooks. Every call site uses the soft `eval { ... }; warn "pve-meta: ..." if $@;` form —
a metadata read/write hiccup never blocks the underlying guest operation.

This tool is the "lifecycle" sibling of `pve-manager-patch/pve-meta-patch` (the web-UI
tab injector) and deliberately reuses its mechanism and CLI shape — read that tool's
README first if you haven't; this document only calls out what's different.

Tested against **pve-manager 9.2.11** / `pve-container 6.1.14` / `qemu-server 9.2.7` /
`libpve-guest-common-perl 6.0.5` on a disposable lab node (`pvemeta-node1`,
`10.10.10.154`).

## Files

- `pve-meta-lifecycle-patch` — bash script, installed as
  `/usr/sbin/pve-meta-lifecycle-patch`. Subcommands: `apply`, `remove`, `verify`,
  `status`.
- `*.diff` (seven files) — unified diffs, one per patched Perl file, generated in
  `docs/LIFECYCLE-PATCHES.md` §7 from real installed source. Shipped verbatim; the tool
  applies them with `patch -p1`, never re-implements the insertions as shell logic.
- `debian/pve-meta.triggers.lifecycle`, `debian/postinst.lifecycle`,
  `debian/postrm.lifecycle` — packaging-glue snippets to fold into the real
  `pve-meta` package's `debian/pve-meta.triggers` / `debian/postinst` / `debian/postrm`
  (which already carry the UI-patch tool's own triggers/apply/remove calls) — these are
  fragments to merge in, not standalone maintainer scripts.

## The seven patched files

| Key | Real path (under the diverted-file's own dir) | Diff file | Package |
|---|---|---|---|
| `AbstractConfig` | `/usr/share/perl5/PVE/AbstractConfig.pm` | `libpve-guest-common-perl_AbstractConfig.pm.diff` | libpve-guest-common-perl |
| `API2-LXC` | `/usr/share/perl5/PVE/API2/LXC.pm` | `pve-container_API2-LXC.pm.diff` | pve-container |
| `LXC-Create` | `/usr/share/perl5/PVE/LXC/Create.pm` | `pve-container_LXC-Create.pm.diff` | pve-container |
| `VZDump-LXC` | `/usr/share/perl5/PVE/VZDump/LXC.pm` | `pve-container_VZDump-LXC.pm.diff` | pve-container |
| `API2-Qemu` | `/usr/share/perl5/PVE/API2/Qemu.pm` | `qemu-server_API2-Qemu.pm.diff` | qemu-server |
| `QemuServer` | `/usr/share/perl5/PVE/QemuServer.pm` | `qemu-server_QemuServer.pm.diff` | qemu-server |
| `VZDump-QemuServer` | `/usr/share/perl5/PVE/VZDump/QemuServer.pm` | `qemu-server_VZDump-QemuServer.pm.diff` | qemu-server |

Each real path is diverted to `<real path>.pve-meta-orig` (the full path, suffixed —
not a flattened basename registry: `API2/LXC.pm` and `API2/Qemu.pm`, and `VZDump/LXC.pm`
and `VZDump/QemuServer.pm`, would otherwise collide), all owned by the diversion package
name `pve-meta`, exactly like the UI-patch tool's single `index.html.tpl` diversion.

## Mechanism

1. **`dpkg-divert --package pve-meta --add --rename --divert <path>.pve-meta-orig
   <path>`** per file, the first time `apply` touches it. From then on, `<path>.pve-meta-orig`
   always holds the pristine upstream content (including across future
   `libpve-guest-common-perl`/`pve-container`/`qemu-server` upgrades — dpkg keeps
   routing writes there), and `<path>` is ours to (re)write.
2. **`patch -p1`**, not bespoke `awk`/`sed`. Each diff has up to six non-contiguous
   insertion points per file (two of them, in `API2/LXC.pm`, are inside
   near-identical-looking sibling clone-abort-cleanup blocks) — a single-anchor
   `grep`/`awk` needle approach doesn't generalize to that; three-line unified-diff
   context does, robustly, and fails loudly (non-zero exit) instead of silently
   misfiring if an anchor moved. `patch -p1 --dry-run` runs first; only on a clean
   dry-run does `apply` do the real `patch -p1`, and both runs happen in a scratch
   copy under a temp dir — the diverted pristine file is never touched by `patch`
   itself, only ever read.
3. **`perl -I <perl5 root> -c <scratch file>`** gates installation. This is strictly
   stronger than the UI-patch tool's grep-only `verify` — a misapplied hunk in Perl can
   produce a file that "greps fine" (the marker string is present) but doesn't compile.
   Only the **last line** of `perl -c`'s combined output is trusted as the verdict
   (`grep -Eq 'syntax OK$'`): Perl always prints the pass/fail verdict last, and a
   pre-existing `Subroutine ... redefined` warning during compilation (present on the
   *unpatched* originals too, per `docs/LIFECYCLE-PATCHES.md`'s intro — a harmless
   `use base`/classic-plugin artifact of `-c`-checking a file outside its normal
   `require` chain) must never be mistaken for a failure.
4. **`install -m 0644 <scratch file> <real path>`** only after both gates pass.

## Subcommands

```
pve-meta-lifecycle-patch [--root <dir>] apply  [file...]
pve-meta-lifecycle-patch [--root <dir>] remove [file...]
pve-meta-lifecycle-patch [--root <dir>] verify [file...]
pve-meta-lifecycle-patch [--root <dir>] status [file...]
```

`file...` is optional (default: all seven) and each may be given as the short key
(`AbstractConfig`), the diff's basename, the path relative to `/usr/share/perl5`, the
absolute path, or the bare basename (ambiguous for `LXC.pm`/`QemuServer.pm`, which
exist under two different directories each — an ambiguous basename selects *all*
matching entries, not an error).

### `apply`

Per file: divert (if not already, reusing the existing diversion otherwise) → scratch
copy of the pristine backup → `patch -p1 --dry-run` → `patch -p1` for real → `perl -c`
→ `install`. **Best-effort per file**: if one file's dry-run or `perl -c` fails, that
file is left alone (or, if `apply` diverted it moments ago in this same run, the
diversion is rolled back so no half-applied state lingers) and the tool moves on to the
next file — one file's anchors moving on some future point release must not block
patching the other six. Exit status is non-zero if *any* file failed, after all
requested files were attempted. Re-running `apply` is idempotent: it always regenerates
from the pristine backup + diff, never from the (possibly already-patched) live file, so
re-applying never double-inserts.

### `remove`

Per file: mirrors the UI-patch tool's `remove` exactly — delete the live (patched) file,
then `dpkg-divert --remove --rename` to rename the pristine backup back onto the real
path (clearing the live file first, since `dpkg-divert` refuses to rename onto an
existing destination); falls back to `cp -a` from the pristine backup if `dpkg-divert`
itself fails, so the host is never left without the file. Verified end-to-end on the lab
node: `md5sum` of all seven files after `apply` → `remove` is identical to their
pre-`apply` (pristine) `md5sum`.

### `verify`

Read-only. Per file: `patch -p1 --dry-run` against whichever pristine copy is available
(the diverted backup if already applied, else the live file, assumed pristine) and
reports `n`/`total` hunks that would apply cleanly, or a full failure listing including
the `patch` output when any hunk fails. Makes no changes — safe to run before `apply`,
and the intended CI check via `--root` (see below).

### `status`

Per file: whether it's diverted (and by which package — a diversion owned by anyone
other than `pve-meta` is flagged as unexpected and left alone by every other
subcommand), whether the pristine backup is present, whether the live file carries the
`use PVE::RS::Meta;` marker, and whether the live file is byte-for-byte what the shipped
diff would produce from the current pristine (checked via a `patch -p1 -R --dry-run`
reverse-application probe) — a stronger claim than the marker grep alone, since a file
patched by an *older* version of a diff would still carry the marker but fail this
check.

### `--root <dir>`

Resolves every path this tool touches under `<dir>` instead of `/`, and passes
`--root <dir>` through to every `dpkg-divert` invocation. Primarily intended for running
`verify` (and `status`) against an extracted package tree in CI, without a live system
or root — `apply`/`remove` also honor it for the same non-live-tree use case (e.g.
testing this tool's logic against a scratch tree that has its own `dpkg` admin
directory), though the design brief for this tool only requires it for `verify`.

## Why `patch`-based, not bespoke shell insertion logic

See `docs/LIFECYCLE-PATCHES.md` §10 "Why `patch`-based, not a rewrite of every insertion
as bespoke shell logic" for the full argument — in short, this job has up to six
insertions per file, several inside near-identical sibling blocks, which is exactly the
problem multi-line unified-diff context solves and single-needle `grep`/`awk` does not
(and re-implementing every hunk by hand would be both more code and *more* fragile, not
less).

## What was tested on the lab node (10.10.10.154, disposable)

See the top-level task report for the full session log (Perl bindings build/install,
`apply`/`verify`/`status`/`remove` round trip with `md5sum` comparison, and the
CLI-less lifecycle-hook exercise — snapshot/rollback/delsnap/clone/destroy and vzdump
backup/restore for both a container and a VM, including the documented QEMU
disk-having-VM backup gap from `docs/LIFECYCLE-PATCHES.md` §4.2).

## Known limitations / open doubts

- **`verify`'s "pristine" fallback can be wrong if `apply` was never run and the live
  file is *not* actually pristine** (e.g. hand-edited, or already patched by some other
  mechanism) — `verify` has no way to distinguish "genuinely pristine" from "just
  happens to have no diversion recorded" and will happily dry-run against whatever it
  finds. `status`'s "in sync w/ diff" reverse-probe is the sharper tool for "is the live
  file exactly what our diff produces" once a diversion does exist.
- **Ambiguous basename selection is silently inclusive, not an error.** Passing `LXC.pm`
  selects both `API2-LXC` and `VZDump-LXC` (and `QemuServer.pm` selects both
  `QemuServer` and `VZDump-QemuServer`). This is intentional — a stricter "ambiguous,
  please disambiguate" error would be more surprising for the common case of running the
  tool with no file arguments at all (all seven) — but worth knowing if you intend to
  target exactly one of a same-basename pair; use the key or the path relative to
  `/usr/share/perl5` instead.
- **`on_clone`'s die-on-conflict is swallowed by the soft eval, same as every other
  hook** — see `docs/LIFECYCLE-PATCHES.md` §3.4 for the documented trade-off (a stale
  metadata document at the target vmid is left in place rather than blocking the
  clone); this tool has no opinion on that trade-off, it only installs the code that
  embodies it.
- **The QMP `backup` command's fixed parameter set** means disk-having QEMU VMs cannot
  carry the metadata blob inside a vzdump/PBS backup at all today (`docs/
  LIFECYCLE-PATCHES.md` §4.2) — this is a `pve-qemu-kvm`/QEMU-side limitation, not
  something any Perl-only patch (or this tool) can close. Containers and diskless VMs
  are unaffected.
