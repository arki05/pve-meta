# 014 — Schemas are advisory unless a prefix says `enforce: true`

**Status:** accepted (2026-09-11).

## Context

A prefix's schema drove the editor's rows, markers and hovers, and the server ignored
it: the one lint decided what was storable, deliberately, so an operator whose schema
had drifted could never lock the administrator out of a document. That left an
operator with no way to rely on the shape of its own prefix through the API.

## Decision

`enforce: true` on a prefix makes `put_document` refuse — 422, naming the paths — a
write that would leave that prefix's subtree not matching its schema, for the findings
the write introduces or touches (`shape::introduced`), never for what was already wrong
elsewhere. `force=1` stores the result anyway, and it is available to anyone who may
write: enforcement makes a mismatch a deliberate act, not an impossible one, so the
lock-out the advisory rule protected against cannot happen. The editor's "Save anyway"
tick sends `force=1`; findings carry `enforced` so the banner says which lines will be
refused. Format checks (`dns-name`, `ipv4`, ...) are never enforced, because only the
editor holds PVE's validators for them and the server must not refuse on a guess.

## Consequences

The server's `check_value` (type, enum, minimum, maximum) is now a gate where a prefix
asks for one, so its completeness matters. A scoped principal cannot be locked out of
its own prefix by another prefix's schema, since it can only change paths inside its
own. Registry documents have their own gate (003) and are not affected.
