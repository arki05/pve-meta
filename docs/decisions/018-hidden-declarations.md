# 018 — A declaration can say it is not a row; `hidden` and `enforce` inherit

**Status:** accepted.

## Context

A prefix's schema both types a document and renders what it lacks as a greyed row. That
scales badly: a vocabulary of a few hundred keys turns every unset one into a greyed
row, burying what the guest actually says. `enforce` has a matching problem: a schema
often has a modelled part worth refusing bad writes into, and a passthrough subtree
with no shape to check, and one flag cannot say both.

## Decision

A schema node may say `hidden: true`: the editor offers that path as a row only once
something is stored there. A node may also carry `enforce`, overriding the prefix's own
value for its subtree. Both are **inherited**, with an explicit value at any depth
winning. Neither decoration hides *stored* data — a set key keeps its row, type,
default and description — nor is either seen by validation or `enforce`'s findings
(014): a hidden declaration is still a declaration.

## Consequences

The prefix's own row always shows. Un-hiding a key is additive, so the rule can only
ever reveal more than it did.
