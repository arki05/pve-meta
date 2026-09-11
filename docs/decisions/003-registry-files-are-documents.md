# 003 — Prefix and permission files are documents

**Status:** accepted (2026-09-09).

## Context

The editor's tree, its markers, its diff, the digest compare-and-swap and the version
poll are all written against *a document*. Editing a prefix file needed all of them.
The alternative was a second read/write path beside the first, which is the shape of
every wrong-result bug this project had had.

## Decision

`prefixes/<name>` and `permissions/<name>` are document ids, reachable at
`/meta/prefixes/{name}` and `/meta/permissions/{name}` with the same parameters a guest
document takes. Four things are specific to them: writes land in the cluster directory
(editing a packaged file creates the override, deleting the override reverts); the
result must parse as its kind, checked with the loader's own parser on every write,
because the loader deliberately skips a malformed file and a 200 must never make one;
no permission ever reaches them; and they move the version token.

The two file formats are themselves described as schemas (`GET /meta/schemas`), so the
editor renders a registry document with the same code that renders a guest document
with its prefixes. The meta-schema is an affordance, not the validator; a test keeps
its required keys equal to what the parser refuses to do without.

## Consequences

One "New" dialog, one editor window, one diff for three kinds of file. A file that does
not load is still *listed*, with its error, because a prefix that silently ceased to
exist was the one failure mode with no observable symptom.
