# 007 — Key order is kept on disk and is not meaning

**Status:** accepted (2026-09-11). Supersedes "key order is data".

## Context

serde's `preserve_order` keeps a parsed document's key order through a dump, and
`view::replace` keeps a key's slot, so order survives on disk for free. The question was
whether anything *treats* order as data. The design once said yes, and a chain of
mechanics followed: an ordered equality (`model::same_ordered`), a replay check in
`EditSet::between` that fell back to replacing the whole document when only order had
changed, a rule that the editor must read YAML and never JSON (a Perl hash has no order,
verified: the same document came back in different orders from different workers), and a
note that reordering is not access-controlled because it touches no path.

Nobody observed order except in text mode and in the file. The tree sorts its rows.
Consumers reading `format=json` never got it. Hook scripts got it and did not care. Every
mechanism above existed to protect something no one looked at, and each was a place two
notions of "the same document" could disagree.

## Decision

Order is a courtesy, not a value. The store writes keys back in the order it read them
and never sorts; `view::replace` keeps a key's slot. Equality is `Value`'s own: maps
compare as sets. A pure reordering stages nothing in the editor, `EditSet::between` has
no replay fallback, `same_ordered` is gone, and the wasm `same` is plain equality. A
reordering typed in Text mode is still written when applied *from* Text mode, because
that path sends the buffer, not the model; switching to the tree drops it.

The editor still reads the document as YAML, now for booleans (`format=json` renders
them as `1`/`0`) and so that an Apply at the root view writes back the order the file
already has rather than churning it.

## Consequences

One notion of equality. If sorted files are ever wanted, that is one line in
`format::dump`. Returning `data` as a JSON string, which would carry booleans and let
the editor read JSON, remains a separate, open choice (006).
