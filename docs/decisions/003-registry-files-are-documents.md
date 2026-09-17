# 003 — Prefix files are documents

**Status:** accepted.

## Context

The editor's tree, markers, diff and digest compare-and-swap are all written against *a
document*. Editing a prefix file needed all of them, and a second read/write path
beside the first is the shape of every wrong-result bug this project has had.

## Decision

`prefixes/<name>` is a document id, reachable at `/meta/prefixes/{name}` with the same
parameters a guest document takes. Writes land in the cluster directory (editing a
packaged file creates the override, deleting it reverts) and move the version token;
the result must parse as a prefix, checked with the loader's own parser on every write,
because the loader deliberately skips a malformed file and a 200 must never make one.

## Consequences

One "New" dialog, one editor window, one diff. A file that fails to load is still
listed, with its error: silently ceasing to exist had no observable symptom.
