# 021 — Comment keys are notes: a read leaves them out unless it asks

**Status:** accepted (2026-09-17).

## Context

A key ending in `__` was ordinary data: every read returned it and every export dumped
it. It is a note for the person editing the document, and it leaked into everything
that consumes one. pve-compose had to strip notes itself before rendering a guest's
document, and an operator that publishes a subtree as a service's configuration file
would write them into that file. Every consumer either learns the rule or carries the
notes along, and the ones that do not know about it are the ones that ship them.

## Decision

Comment keys are left out of every read unless it asks for them. A read without
`comments=1` omits them at any depth, in `data` and in `text`, which is then the
canonical dump of what is left rather than the file's text; a view naming one is a
400. `digest` stays the file's, so the compare-and-swap is unaffected. With
`comments=1` a read is what it was.

A write must not destroy what its caller never saw. A `replace` without `comments=1`
may carry no comment key, and the stored notes under its view whose subject it keeps
are put back before it is planned (`view::keep_comments`); a note whose subject is
dropped goes with it, and so does a note in a list member that changed, since a
position says nothing about what a note was written about. A kept note is not a
change, so it needs no permission (002) and meets no enforced schema (014). A kept
note goes back where it stood, so a stripped read written back is the same bytes: no
rewrite, and no version change. Refusing a note in such a body, rather than ignoring
it, is the stricter rule and the simpler one: a caller that wants to write notes says
so, and nothing it sent is silently discarded. With `comments=1` the payload is the
subtree, notes included. `merge` names what it changes and is unchanged, and a key's
note goes with the key on a `DELETE` or a `merge` to `null`, so a note never outlives
its subject to be revived by the next key of that name. The editor owns the description
column and always sends `comments=1`.

What carries the file rather than a view for a consumer — the backup block, `scan-notes`,
the version detail — is unaffected.

## Alternatives

* **Strip per consumer.** What pve-compose did. Every operator reimplements one rule,
  and the one that forgets publishes the notes; the store is the one place that knows
  what a comment key is.
* **A separate notes tree** — a sidecar document, or a reserved top-level key. Notes
  would drift from what they describe on every rename or delete, a view's notes would
  need a second address and a second permission check, and 002's rule would have two
  documents to diff.
* **Refuse a replace when the stored subtree has notes.** Simple, and it breaks every
  automation the moment a person annotates a document it writes.
* **Require `comments=1` for every replace.** Every script would have to carry the
  notes through its own model, or send the flag and silently drop them.
* **YAML `#` comments.** Invisible to every reader for free, and lost on the first
  write, since documents are written canonically from their value (006); the tree
  editor could never show or edit one.

## Consequences

A client that does GET, modify, PUT without `comments=1` cannot edit notes: it never
sees them, and a note in its body is a 400. It keeps them, which is the point. Existing
clients see a wire change: reads no longer carry notes, a replace naming or carrying
one is refused, and a full reader's root YAML is a canonical dump instead of the file's
own text until it asks. `GET /meta/guests?has=` naming a comment key is a 400.
