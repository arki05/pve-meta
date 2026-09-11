# 005 — A read never fails on a document's own content

**Status:** accepted (2026-09-08).

## Context

The store once linted on read, and later still parsed on read. Either way one bad key,
or one tab in a hand-edited file, made every request touching that document a 400 for
every principal, root included — and unrepairable through the API, because every write
reads the document before planning. While permissions lived in `datacenter.yaml`, that
was a cluster-wide outage.

## Decision

Reads never lint and never fail on content. A file that is not valid YAML, is above the
read cap, or parses to something that is not a map is **unrecoverable**: it comes back
as the empty document with its real digest and a `parse_error`, so a full reader can
see the raw text (`format=yaml`) and repair it. The only writes accepted against an
unrecoverable document are a root `replace` and a root `DELETE`, by a full writer,
because anything narrower would be planned against the empty document and silently
discard the file. The condition is per document, never cluster-wide.

Strict validation belongs to the content being written: the one lint runs on the
planned document.

## Consequences

`MetaStore::read` reports `parse_error` instead of an error; the store has no
`exists()`-then-act pair left, because a file can vanish between two syscalls and that
must be a 404, not a 500. Reads above 4 MiB are never parsed; their identity is a
surrogate over size and mtime so a poll does not hash megabytes every tick.
