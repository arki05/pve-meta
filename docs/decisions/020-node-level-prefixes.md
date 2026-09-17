# 020 — A node's own prefix files reach the guests on that node

**Status:** accepted (2026-09-17).

## Context

Prefix files came from two layers, packaged and cluster, and every one of them described
every guest its selector matched, wherever that guest ran. Some facts are not the
cluster's to state: which GPUs a host has and what a guest may ask for from them, which
bridge a node uplinks on, which local storage it has. A cluster-wide schema for those
either lists every node's values at once, and then validates a guest against hardware
it is not on, or says nothing useful. PVE keeps per-node configuration in
`/etc/pve/nodes/<node>/`, next to the guest configs, and pmxcfs replicates it like the
rest of `/etc/pve`.

## Decision

A third directory, `/etc/pve/nodes/<node>/meta.d/prefixes/`, above the cluster's. A
guest's prefix set is the packaged, cluster and **its current node's** files, resolved
by name with the rule the two lower layers already had: whole file, precedence by
presence, a malformed node file shadows the file of its name and is the failure listed
for it. Schemas still never merge; the layers compose only through most-specific-wins,
so a cluster `gpu` and a node `gpu.devices` both apply on that node. The node is the
vmlist's answer at request time, so nothing is recorded about it anywhere and a migrated
guest simply gets the other node's set. This narrows 001's "one prefix is exactly one
file": for any one guest it still holds, while the `all=1` listing shows several files
of one name.

A node file is a registry document like the other two (003),
`nodes/<node>/prefixes/<name>`, written with `Sys.Modify` on `/nodes/<node>`. Its node
must be of PVE's node-name format, and no path is built from any other name; only
creating a file needs the node in the nodelist. There are no node-level permission files: access stays the
cluster's (001), since a permission that changed with the guest's node would make who
may write a document depend on where it runs.

## Alternatives

* **Per-node companion files in the cluster directory**, keyed by node name in the key
  path (`gpu.pve1.devices`) or in the file name. It needs no new directory and no node
  in any request, and it is wrong in the one respect that matters: a guest on `pve2` is
  still offered, validated against and enforced by `pve1`'s declarations, because a key
  path says nothing about where the guest runs.
* **Schema merging across layers** — a node file adds or narrows properties of the
  cluster file of the same name. It makes the effective schema of a path a computation
  over several files instead of a lookup, which is what 001 refused for nested prefixes
  (shape has one owner), and a broken node file would have to either break the merge or
  silently fall back, which precedence by presence refuses. Composing by a more specific
  prefix name gets the useful half of merging with no new rule.
* **Node as a selector kind** (`selector: {node: pve1}`) on a cluster file. Selectors
  decide reach, not precedence, so two files of the same name for two nodes could not
  coexist — the file name is the prefix — and every per-node variant would need its own
  prefix name, which is the key-path option again.

## Consequences

**A guest's effective schema changes when it migrates.** Declared rows, hidden keys and
enforcement follow the node; the document does not. A key declared only on the old node
becomes an ordinary present-but-undeclared key, and a value that was valid there may be
a finding on the new node, answered for only by the write that next touches it (014). A
guest's scoped version token hashes its node name, so an open editor reloads on
migration even between nodes whose directories hold the same files.

**What a guest looks like in the editor depends on its node.** The guest tab asks for
its prefixes by the guest's id and the server reads the node from the vmlist, so the
reload a migration triggers shows the new node's set. `GET /meta/prefixes` stays the
cluster-wide set by default; only `all=1` adds every node's files, for administration,
never a set a guest has.

The version token and the `all=1` listing walk every node directory, which grows with
the nodes in the cluster, not the guests. A removed node's leftover files are ordinary
files: listed, openable and removable.
