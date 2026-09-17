# 025 — A node's schema is an override inside the prefix file

**Status:** accepted. Supersedes 020.

## Context

020 gave nodes their own directory, `/etc/pve/nodes/<node>/meta.d/prefixes/`, layered
under the cluster's and resolved by file name. It cost a third directory layer, a
document kind, an endpoint family variant (`all=1`), a lock-naming branch, and a
version token that hashed the node name — for the same fact a much smaller shape can
hold: which node a guest is on, which the vmlist already answers.

## Decision

A prefix file gets one optional key, `nodes`, keyed by node name; `nodes.<node>` fields
(`schema`, `enforce`, `hidden`) replace the file's top-level ones for guests on that
node, wholesale, never merged (§3) — the same "most specific wins" rule the file itself
already applies to its own subtree. One file, one document, one write, one digest.

## Consequences

The effective schema for a guest stays a lookup: read the prefix file, and if the
guest's node has an entry, use it instead of the top level. No new document kind, no
new lock, no per-node version hashing.
