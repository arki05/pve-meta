# 022 — A store that is not there is an error, not an empty store

**Status:** accepted (2026-09-17).

## Context

Without pmxcfs mounted — stopped, crashed, not yet started at boot — `/etc/pve` is an
ordinary empty directory on the root filesystem. Every walk of it succeeded and found
nothing, so a read was a 404 or an empty document, `version` listed no documents and
`pve-meta ls` printed nothing. An operator reconciling against that answer — removing
the service files of documents that "vanished" — does the damage the answer invites.
The directory walks also treated a directory they could not list, for any reason, as
absent.

## Decision

The core refuses. A `MetaStore` rooted under `/etc/pve` checks, before every operation
including handing out its registry, that `/etc/pve/local` is a symlink: pmxcfs provides
it and nothing else does, and it is what `PVE::Cluster::check_cfs_is_mounted` tests. If
not, `Error::Unavailable`, which the API maps to 503 and the CLI prints and exits 1 on.
One place: every `MetaStore` constructor decides the marker from its root, so the API,
the CLI, the lifecycle hooks and an operator that opens a store all get the check
without asking for it. `Registry::from_env` and the free registry loaders are building
blocks and check nothing; the store hands its registry out behind the check. Every
directory walk, in the store and in the registry, reads only *not found* as absent, and
a directory that cannot be listed fails the answer. One entry that cannot be looked at
costs that entry: a registry file becomes a listed failure, a node directory is skipped
with a warning.

This does not contradict 005. A read never fails on a document's *content*: a file that
is there and does not parse is still per-document and repairable. Whether the store is
there at all is not content, and there is no document to repair.

`PVE_META_CLUSTER_MARKER` names the marker for a store whose root is not
`/etc/pve`, so the refusal is testable under a temporary root.

## Consequences

Every operation costs one `lstat` of a pmxcfs symlink, answered from memory. A pmxcfs
that goes away mid-request answers that `lstat` with `ENOTCONN`, which is also refused.
The lifecycle hooks warn and
carry on, as they do for any error, and a guest operation that needs pmxcfs fails on its
own.
