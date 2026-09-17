# 005 — A read never fails on a document's own content

**Status:** accepted.

## Context

The store once linted, and later parsed, on read. One bad key, or one hand-edited typo,
made every request touching that document a 400 for every principal, root included —
and unrepairable through the API, since every write reads the document before planning.

## Decision

Reads never lint and never fail on content. A file that is not valid YAML, is above the
read cap, or parses to something that is not a map is **unrecoverable**: it comes back
as the empty document with its real digest and a `parse_error`, so a full reader can see
the raw text (`format=yaml`) and repair it. Only a root `replace` or `DELETE` is
accepted against it, because anything narrower would be planned against the empty
document and silently discard the file. The condition is per document, never
cluster-wide. Strict validation belongs to the content being written: the one lint runs
on the planned document.

## Consequences

`MetaStore::read` reports `parse_error` instead of an error, with no `exists()`-then-act
pair, since a file can vanish between two syscalls and that must be a 404, not a 500.
