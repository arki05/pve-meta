# 007 — Key order is kept on disk and is not meaning

**Status:** accepted; the editor's reorder diff is open.

## Context

serde's `preserve_order` keeps a parsed document's key order through a dump, and
`view::replace` keeps a key's slot, so order survives on disk for free. The question is
whether anything *treats* order as data. The design once said yes (§2: "key order is
part of the file"), and a chain of mechanics followed: an ordered equality
(`model::same_ordered`), a replay check in `EditSet::between` that falls back to
replacing the whole document when only order changed, a rule that the editor reads
YAML and never JSON (a Perl hash has no order, verified: the same document came back
in different orders from different workers), and a note that reordering is not
access-controlled because it touches no path.

Nobody observes order except in text mode and in the file. The tree sorts its rows.
Consumers reading `format=json` never get it. Hook scripts get it and do not care.

## Decision

Order is a courtesy: the store writes keys back in the order it read them and never
sorts. No lookup, selector or permission depends on it, and it is not access-controlled
(002). What remains open is whether the editor should still treat a text-mode reordering
as a change worth writing, which is what `same_ordered` and the `between` fallback
exist for. Demoting that — a reorder becomes "no changes" — would delete those, drop the
YAML-only-read rule, and make `format=json` a full-fidelity read once booleans are fixed
(see the note in 006). It is deferred, not decided.
