# 021 — Comment keys are notes: a read leaves them out unless it asks

**Status:** accepted.

## Context

A key ending in `__` was ordinary data: every read returned it and every export dumped
it. It is a note for the person editing the document, and it leaked into everything
that consumes one — an operator publishing a subtree as a service's file would write
notes into it, and every other consumer had to learn the rule or carry them along.

## Decision

A read without `comments=1` omits every `k__` at any depth, in `data` and in `text`,
which is then the canonical dump of what is left; a view naming one is a 400. A
`replace` that did not ask for comments must not destroy notes it never saw: it keeps
the stored `k__` of every map key `k` it keeps, drops one whose subject it drops, and
keeps nothing inside a list. Deleting `k` deletes `k__`. With `comments=1` a read and a
write are what they were: the payload is the subtree, notes included.

## Consequences

A client that never asks for `comments` cannot see or edit notes, and keeps them by
construction. What carries the whole file rather than a view — the backup block,
`scan-notes` — is unaffected.
