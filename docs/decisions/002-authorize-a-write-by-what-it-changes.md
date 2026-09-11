# 002 — A write is authorized by what it changes, not by where it is aimed

**Status:** accepted (2026-09-10).

## Context

The view named in a request used to have to sit inside a writable scope, and the root
view demanded full write outright. That refused writes that violated nobody's
permissions: a permission file has `rules`, plural, so holding `rw` on two prefixes and
editing one key in each is ordinary, and the narrowest view covering both is the root.
A key reordering was the same story: it changes no path, so it can only be expressed as
a whole-document write, and it was a 403.

## Decision

The mutation is planned against a copy of the stored document, and every path the plan
touches — values changed, keys added, keys removed — must be covered by full write or an
`rw` scope. The view is where the write is aimed, not what it may do.

Two request-shaped refusals remain, neither about content:

* The caller must be able to **read the view it names**; otherwise the content check is
  a read oracle (replace a key you cannot read with a guess, and 200 versus 403 tells
  you whether the guess was right). This is also what keeps a scope-only principal out
  of the root view.
* The caller must hold **some write permission** on the document; key order is not a
  path, so a pure reordering touches nothing and would otherwise let a read-only
  auditor rewrite the file.

`full_write` short-circuits both. A document that cannot be read back is the one
exception: repairing it as a whole needs full write, because there is no stored content
to check the change against.

## Consequences

Key order is not access-controlled: anyone who may write something may reorder keys,
including keys they may not otherwise touch (see 007). The touched list a view
operation reports must be complete — creating or removing structure must never report
nothing — and the tests for `view::replace`, `merge` and `remove` pin that.
