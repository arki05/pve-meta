# 014 — Schemas are advisory unless a prefix says `enforce: true`

**Status:** accepted.

## Context

A prefix's schema drove the editor's rows, markers and hovers, and the server ignored
it: the one lint decided what was storable, deliberately, so a schema that had drifted
could never lock the administrator out of a document. That left an operator with no way
to rely on the shape of its own prefix through the API.

## Decision

`enforce: true` on a prefix makes `put_document` refuse — 422, naming the paths — a
write that leaves that prefix's subtree not matching its schema, for findings the write
introduces or touches, never what was already wrong elsewhere. `force=1` stores the
result anyway: enforcement makes a mismatch a deliberate act, not an impossible one.
Format checks (`dns-name`, `ipv4`, ...) are never enforced, since only the editor holds
PVE's validators for them and the server must not refuse on a guess.

## Consequences

`check_value` (type, enum, minimum, maximum) is a gate where a prefix asks for one, so
`parse_prefix` refuses a known keyword with a value that would silently constrain
nothing (`type: interger`, `enforce: yes`); unknown keywords still pass.
