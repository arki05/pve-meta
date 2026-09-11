# 018 — A declaration can say it is not a row

**Status:** accepted (2026-09-11).

## Context

A prefix's schema does two jobs at once. It types and validates what a document
has, and it renders what the document lacks as a greyed row, so a key an operator
declared is discoverable before anyone sets it. Both are worth having, and the
second one scales badly: at five declared keys it is the feature, and at two
hundred -- the size a real Traefik vocabulary reaches -- every unset key becomes a
greyed row and what the guest actually says is buried in them.

Nested prefixes already split a large vocabulary into governed subtrees, and they
should be the first answer. They do not finish the job: a single subtree still has
its own long tail of options nobody sets.

## Decision

A schema node may say `hidden: true`. The editor then does not offer that path as
a row before something is stored there. It is **inherited**, and an explicit
setting wins at any depth, so a subtree is hidden at its root and the two keys
worth showing are named inside it. The walk always descends, so that override
needs no lookahead.

Two things it deliberately does not do.

It never hides data. A hidden key that *is* set has its row from the document, and
still takes its type, enum, range, default and description from the schema --
`hidden` decorates a row that exists and never creates one. A setting that could
hide stored content would be worse than the problem it solves.

It never reaches validation. Findings and `enforce` (014) ignore it entirely, the
way `multiline` and `format` are ignored: a hidden declaration is still a
declaration, and a value under it is refused exactly as a shown one would be.

## `enforce` follows the same rule

`enforce` was a prefix-level flag: the whole subtree was refused on, or none of
it. The case that breaks is the one a real vocabulary has -- a modelled part
worth refusing bad writes into, and a passthrough subtree that by definition has
no shape to check. Those two cannot coexist under one flag, and a partial schema
without an escape hatch is not honest.

So a schema node may carry `enforce` too, inherited the same way, with the
prefix's own flag as the root default. `enforce: true` on the prefix and
`enforce: false` on the passthrough subtree says exactly what an operator means.
The anti-lockout property is unchanged: `force=1` remains available to anyone who
may write, so enforcement is still a deliberate act rather than an impossible one.

Both flags also exist at the prefix level, which is just the root default of the
inherited value -- `hidden: true` there is "this prefix offers no declared-but-unset
rows at all", which is what a vocabulary wants and what five keys do not.

## Consequences

The second extension to the PVE::JSONSchema dialect, after `multiline`. Both are
editor hints the server neither reads nor validates against, which is the bar for
adding one.

Un-hiding is additive, so the rule can only ever reveal more than it did. If a
richer version is ever wanted -- sections, groupings, ordering for a generated
form -- it belongs with that feature and not here; this decision is about which
rows exist, not how they are laid out.
