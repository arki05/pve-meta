# Decisions

Why the rules in [`../DESIGN.md`](../DESIGN.md) are what they are: the context, the
alternatives that were tried or considered, and what each decision costs. One file per
decision, numbered in the order they were made. A decision that is later reversed gets a
new record; the old one is deleted, since git remembers it.

| # | Decision | Status |
|---|---|---|
| [003](003-registry-files-are-documents.md) | Prefix files are documents | accepted |
| [005](005-reads-never-fail-on-content.md) | A read never fails on a document's own content | accepted |
| [006](006-yaml-on-disk-strict-and-canonical.md) | YAML on disk, read strictly, written canonically; JSON is a wire format | accepted |
| [007](007-key-order-is-kept-not-meaning.md) | Key order is kept on disk and is not meaning | accepted |
| [008](008-lifecycle-is-one-patched-file.md) | The guest lifecycle is one patched file; clone is not carried | accepted; backup half superseded by 019 |
| [009](009-no-sweeper.md) | No garbage collector: hooks on create and destroy, a CLI for orphans | accepted |
| [011](011-the-browser-reimplements-nothing.md) | The editor runs the core as wasm and reimplements no rule | accepted (see also `../WASM-CORE.md`) |
| [012](012-extjs-not-pwt.md) | The editor is plain ExtJS, not pwt/Yew | accepted |
| [013](013-no-datacenter-document.md) | There is no datacenter-level document | accepted |
| [014](014-schemas-advisory-with-enforce-opt-in.md) | Schemas are advisory unless a prefix says `enforce: true` | accepted |
| [015](015-audit-line-per-write.md) | Every write logs one audit line | accepted |
| [016](016-write-cli-for-root.md) | The CLI writes, for root on the node, checking nothing | accepted |
| [017](017-data-is-a-native-structure.md) | `data` on a read is a native structure, in PVE's spelling | accepted |
| [018](018-hidden-declarations.md) | A declaration can say it is not a row; `hidden` and `enforce` inherit | accepted |
| [019](019-backup-carries-the-document-in-the-notes.md) | A backup carries the document in the archive's copy of the guest's notes | accepted |
| [021](021-comment-keys-hidden-unless-asked.md) | Comment keys are notes: a read leaves them out unless it asks | accepted |
| [022](022-unavailable-store-is-an-error.md) | A store that is not there is an error, not an empty store | accepted |
| [023](023-publish-is-a-separate-package.md) | Publishing into containers is a separate package that pushes files one way (now guest-files) | superseded by 029; deleted with the package in 0.4 |
| [024](024-access-is-pves-acls.md) | Access is PVE's ACLs | accepted, supersedes 001, 002, 004 |
| [025](025-a-nodes-schema-is-an-override-inside-the-prefix-file.md) | A node's schema is an override inside the prefix file | accepted, supersedes 020 |
| [026](026-the-editor-writes-each-edit.md) | The editor writes each edit | accepted, supersedes 010 |
| [027](027-the-loader-gates-nothing.md) | The page loader gates nothing and every page is a script | accepted |
| [029](029-pve-meta-writes-nothing-into-guests.md) | pve-meta writes nothing into guests; guest-files is deprecated | accepted, supersedes 023 |

Numbers missing from the table are records deleted when their decision was reversed;
the successor in the table is what replaced them. **001** (a prefix and a permission
are two files), **002** (a write is authorized by what it changes) and **004**
(permissions are pve-meta's own files, not PVE ACL paths) by
[024](024-access-is-pves-acls.md); **010** (edits are staged, the tree and the text
editor two views of one edit set) by [026](026-the-editor-writes-each-edit.md); **020**
(a node's own prefix files reach the guests on that node) by
[025](025-a-nodes-schema-is-an-override-inside-the-prefix-file.md). Git has the deleted
text.

Related records kept in their own files: [`../WASM-CORE.md`](../WASM-CORE.md) (how the
core reaches the browser) and [`../LIFECYCLE.md`](../LIFECYCLE.md) (the
five lifecycle hooks, backup and restore, and why clone is not carried).
