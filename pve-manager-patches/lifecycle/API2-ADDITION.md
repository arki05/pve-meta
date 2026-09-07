# Addition for `pve-meta-lifecycle-patch`: native `PVE::API2::Meta` registration

Written by the `PVE::API2::Meta` / native-API agent (`docs/NATIVE-API-SPEC.md`) for the
coordinator to merge into `pve-meta-lifecycle-patch` and its packaging glue. Do not hand
re-derive the diff insertions from prose below — the shipped `.diff` file is the source
of truth; this document only describes the table row, marker, and trigger wiring needed
to fold it into the seven-file tool as an eighth entry.

## New file

`pve-manager-patches/lifecycle/pve-manager_API2.pm.diff` — a unified diff (`diff -u`,
`a/PVE/API2.pm` / `b/PVE/API2.pm` headers, same style as the existing seven) generated
against the real `/usr/share/perl5/PVE/API2.pm` fetched from the lab node (PVE 9.2.11,
pve-manager 9.2.11). It adds exactly two things, both inside `package PVE::API2`:

1. `use PVE::API2::Meta;` in the "preload classes" block.
2. A `__PACKAGE__->register_method({ subclass => "PVE::API2::Meta", path => 'meta' })`
   block, placed immediately after the existing `PVE::API2::Pool` (`path => 'pools'`)
   registration and before the `index`/`version` method definitions.

No other changes. `PVE::API2::Meta` needs no addition to `PVE::API2`'s top-level
`index` handler — that handler already builds its `subdir` list by iterating
`method_attributes()` for any entry with a `subclass` key (see the fetched file), so
`meta` appears in `GET /api2/json/` automatically once this diff is applied.

## File-table row

Add to `LIFECYCLE_FILES` in `pve-manager-patches/lifecycle/pve-meta-lifecycle-patch`:

```
"API2|PVE/API2.pm|pve-manager_API2.pm.diff"
```

Package: **pve-manager** (unlike the existing seven entries, which belong to
`pve-container`/`qemu-server`/`libpve-guest-common-perl` — hence the `pve-manager_`
diff-filename prefix, matching the tool's existing `<owning-package>_<File>.pm.diff`
naming convention). The diversion owner stays `pve-meta` (the script's `$PKG`), same as
every other entry — only the *upstream* package that ships the real file differs.

## Marker generalization needed

The tool currently greps a single hardcoded `PATCH_MARKER='use PVE::RS::Meta;'` for
`verify`/`status` across all seven files. This eighth file's diff does **not** add that
line (`PVE::API2::Meta` calls into `PVE::RS::Meta` itself, but `PVE/API2.pm` only ever
gains `use PVE::API2::Meta;` + the `register_method` block) — so `verify`/`status`
against the `API2` entry needs its own marker string, not the shared one. Simplest fix:
add a fourth `|`-separated field to each `LIFECYCLE_FILES` row (marker text), defaulting
existing rows to today's `use PVE::RS::Meta;` and setting `API2`'s to
`subclass => "PVE::API2::Meta"` (present verbatim in the diff's added block, and not a
substring any unrelated file would already contain). Everywhere the script currently
references the global `PATCH_MARKER` in `is_patched`/`verify`/`status`-style checks,
read the row's own field instead.

## Trigger line

Already present in the real `debian/pve-meta.triggers` (added ahead of time, with a
comment noting the diff didn't exist yet): `interest-noawait /usr/share/perl5/PVE/API2.pm`.
No change needed there — just drop the now-stale "did not exist yet" comment above it,
since `pve-manager-patches/lifecycle/pve-manager_API2.pm.diff` now exists. If
`pve-manager-patches/lifecycle/debian/pve-meta.triggers.lifecycle` (the fragment file
meant to be folded into the real one, per this dir's `README.md`) is still tracked
separately from the real `debian/pve-meta.triggers`, add the same line there too:

```
interest-noawait /usr/share/perl5/PVE/API2.pm
```

## README.md

Once merged, add an eighth row to the "files" table in
`pve-manager-patches/lifecycle/README.md`:

| Key | Real path | Diff file | Package |
|---|---|---|---|
| `API2` | `/usr/share/perl5/PVE/API2.pm` | `pve-manager_API2.pm.diff` | pve-manager |

and note in that file's prose that it's now eight files, not seven (title, intro
paragraph, "The seven patched files" heading and its "all owned by the diversion
package name `pve-meta`" sentence).

## Verified against

Applied and verified by hand on the lab node as part of live-testing
`PVE::API2::Meta` (see the native-API agent's own report): `dpkg-divert --package
pve-meta --add --rename --divert /usr/share/perl5/PVE/API2.pm.pve-meta-orig
/usr/share/perl5/PVE/API2.pm`, write the patched copy, `perl -c` clean,
`systemctl restart pvedaemon pveproxy`, then `GET /api2/json/meta/version` (and the
rest of the `/meta` tree) all reachable through the real pveproxy on port 8006.
