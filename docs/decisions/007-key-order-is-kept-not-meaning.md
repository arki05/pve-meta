# 007 — Key order is kept on disk and is not meaning

**Status:** accepted. Supersedes "key order is data".

## Context

serde's `preserve_order` keeps a parsed document's key order through a dump, so order
survives on disk for free. The design once treated order as data, and a chain of
mechanics followed: an ordered equality, a replay check that fell back to replacing the
whole document when only order had changed, a rule that the editor must read YAML and
never JSON. Nobody observed order except in text mode and in the file; the tree sorts
its rows, and `format=json` readers never got it.

## Decision

Order is a courtesy, not a value. The store writes keys back in the order it read them
and never sorts; `view::replace` keeps a key's slot. Equality is `Value`'s own: maps
compare as sets, so a pure reordering changes nothing. The editor still reads the
document as YAML, for booleans (017) and so that a root write puts back the order the
file already has rather than churning it.

## Consequences

One notion of equality, used everywhere a document is compared. If sorted files are
ever wanted, that is one line in `format::dump`.
