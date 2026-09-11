# 010 — Edits are staged; the tree and the text editor are two views of one edit set

**Status:** accepted (2026-09-09/10). Superseded on one point by `9320da6` and
`01e5fa8`: the edit set is not a log a row edit appends to and `between` recovers when
leaving Text mode; it is the difference between the stored document and the planned one,
derived again after every edit, and the log's own operations (`stage`, `under`,
`discard_under`) are gone. Everything else here stands.

## Context

A row edit used to be a write of that one key. A prefix definition's selector is
exactly one of `all` or `tag`, so turning `{all: true}` into `{tag: web}` had no legal
single-key step: dropping `all` was refused, adding `tag` was refused, and the row
editor could only do one at a time. Separately, Tree and Text were two editors with two
models kept apart by rules: switching asked you to discard your work, and Text was
refused outright while anything was staged.

## Decision

`edit::EditSet` in the core is the one model. A row edit stages an edit (`set` is
`view::replace`, `delete` is `view::remove` — the same two operations the server
performs), the tree renders the planned document, and one Apply writes it as a
`replace` at the narrowest view covering every staged path (a lone delete is a
`DELETE`). The text buffer is rendered from the planned document; switching back runs
`EditSet::between` to recover edits from the typed text. Applying from text sends the
buffer, not a dump, so `#` comments survive that one write.

Apply stops to show the diff only when the edit introduces a schema finding, with a
"Save anyway" tick; there is no `dry_run` round trip, because a server refusal is not
advisory and should show as the error it is.

## Consequences

The poll holds off while anything is staged. The staged-row rendering borrows
proxmoxlib's pending-change vocabulary. A staged delete moves the write view one level
up, since a key cannot be removed by replacing it.
