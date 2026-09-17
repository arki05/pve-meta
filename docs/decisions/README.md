# Decisions

Why the rules in [`../DESIGN.md`](../DESIGN.md) are what they are: the context, the
alternatives that were tried or considered, and what each decision costs. One file per
decision, numbered in the order they were made. A decision that is later reversed gets a
new record; the old one stays and is marked superseded.

| # | Decision | Status |
|---|---|---|
| [001](001-prefix-and-permission-are-two-concepts.md) | A prefix and a permission are two concepts, in two files | accepted |
| [002](002-authorize-a-write-by-what-it-changes.md) | A write is authorized by what it changes, not by where it is aimed | accepted |
| [003](003-registry-files-are-documents.md) | Prefix and permission files are documents | accepted |
| [004](004-own-permissions-not-pve-acl-paths.md) | Permissions are pve-meta's own files, not PVE ACL paths | accepted, open to revisit |
| [005](005-reads-never-fail-on-content.md) | A read never fails on a document's own content | accepted |
| [006](006-yaml-on-disk-strict-and-canonical.md) | YAML on disk, read strictly, written canonically; JSON is a wire format | accepted |
| [007](007-key-order-is-kept-not-meaning.md) | Key order is kept on disk and is not a value | accepted |
| [008](008-lifecycle-is-one-patched-file.md) | The guest lifecycle is one patched file; clone and backup are not carried | accepted; backup half superseded by 019 |
| [009](009-no-sweeper.md) | No garbage collector: hooks on create and destroy, a CLI for orphans | accepted |
| [010](010-staged-edits-one-model.md) | Edits are staged; the tree and the text editor are two views of one edit set | accepted |
| [011](011-the-browser-reimplements-nothing.md) | The editor runs the core as wasm and reimplements no rule | accepted (see also `../WASM-CORE.md`) |
| [012](012-extjs-not-pwt.md) | The editor is plain ExtJS, not pwt/Yew | accepted |
| [013](013-no-datacenter-document.md) | There is no datacenter-level document | accepted |
| [014](014-schemas-advisory-with-enforce-opt-in.md) | Schemas are advisory unless a prefix says `enforce: true` | accepted |
| [015](015-audit-line-per-write.md) | Every write logs one audit line | accepted |
| [016](016-write-cli-for-root.md) | The CLI writes, for root on the node, skipping only permissions | accepted |
| [017](017-data-is-a-native-structure.md) | `data` on a read is a native structure, in PVE's spelling | accepted |
| [018](018-hidden-declarations.md) | A declaration can say it is not a row; `hidden` and `enforce` are inherited per schema node | accepted |
| [019](019-backup-carries-the-document-in-the-notes.md) | A backup carries the document in the archive's copy of the guest's notes | accepted |
| [020](020-node-level-prefixes.md) | A node's own prefix files reach the guests on that node | accepted |
| [021](021-comment-keys-hidden-unless-asked.md) | Comment keys are notes: a read leaves them out unless it asks | accepted |

Related records kept in their own files: [`../WASM-CORE.md`](../WASM-CORE.md) (how the
core reaches the browser) and [`../LIFECYCLE.md`](../LIFECYCLE.md) (the
five lifecycle hooks, backup and restore, and why clone is not carried).
