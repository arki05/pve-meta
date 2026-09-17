# 022 — A store that is not there is an error, not an empty store

**Status:** accepted.

## Context

Without pmxcfs mounted, `/etc/pve` is an ordinary empty directory. Every walk of it
succeeded and found nothing, so a read was a 404 or an empty document and `pve-meta ls`
printed nothing. An operator reconciling against that answer does the damage it invites.

## Decision

The core refuses. A `MetaStore` rooted under `/etc/pve` checks, before every operation,
that `/etc/pve/local` is a symlink — pmxcfs provides it and nothing else does. If not,
`Error::Unavailable`, mapped to 503 by the API and printed with exit 1 by the CLI. Every
constructor decides the marker from its root, so every caller gets the check unasked.
Every directory walk reads only *not found* as absent; one that cannot be listed fails
the answer instead. `PVE_META_CLUSTER_MARKER` names the marker for a root that is not
`/etc/pve`. This does not contradict 005: a read never fails on a document's *content*,
and whether the store is there at all is not content.

## Consequences

Every operation costs one `lstat`. The lifecycle hooks warn and carry on, as for any
error.
