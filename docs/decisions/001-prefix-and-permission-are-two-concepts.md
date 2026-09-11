# 001 — A prefix and a permission are two concepts, in two files

**Status:** accepted (revision 6, 2026-09-09).

## Context

Earlier revisions had one object, an "operator registration": a principal, the prefix
it owned, its selector, and its schema, in one file. `authid` was mandatory, so a schema
could not be declared without inventing a principal to own it.

Three things went wrong in use, not in review. The lab configuration had already split
the object by hand: one file carried the schema with `selector: {all: true}`, another
the access with `selector: {tag: traefik}`, because one object could not say both. A
real bug came out of it: declared-but-unset rows were driven by a *permission's* schema,
so a broadly scoped principal painted one operator's rows onto every guest. And two
files naming the same authid silently unioned their scopes, which nobody had decided.

## Decision

Two files, two directories, two nesting rules:

* A **prefix** (`meta.d/prefixes/<prefix>.yaml`) says what a prefix is: selector,
  description, schema. The file name is the prefix, so one prefix is exactly one file.
  Prefixes **shadow**: the most specific one governs a path and schemas never merge,
  because shape has one owner.
* A **permission** (`meta.d/permissions/<name>.yaml`) says who may touch which prefixes
  on which guests. Permissions **accumulate** by containment, because access is a union.

Those two rules cannot live on one object, which is the concrete reason for the split.
Packages may ship a prefix (a declaration) and never a permission (that would be
self-registration); there is no packaged permissions directory, and dpkg cannot write
into pmxcfs, so the rule is enforced by where files live.

## Consequences

The store's vocabulary lost the word *operator*: an operator is an installer, and nothing
at runtime needs the concept. Merging two schemas (`allOf`/`$ref`) is never needed. A
parent prefix that declares a key a child owns is not rejected, only shadowed, since
files are parsed independently.
