# 013 — There is no datacenter-level document

**Status:** accepted. Supersedes the `datacenter.yaml` document.

## Context

Alongside the per-guest documents there was once one for the datacenter, governed by
`Sys.Audit`/`Sys.Modify`. No prefix could describe it (prefixes reach guests only) and
no operator read it. It cost a third ACL mapping, a third endpoint family, a third
document kind the editor had to tell apart, and a sub-tab. An earlier revision had also
used it to hold the registry of who could touch what, which is how one bad key in it
became a cluster-wide outage (005).

## Decision

Guests have documents; nothing else does. Cluster-wide intent that is not about one
guest belongs in the operator that acts on it. The Datacenter panel's Metadata tab shows
the prefix list only. A stray `datacenter.yaml` is ignored: it moves the version token
like any file in the directory and addresses nothing.

## Consequences

Two document kinds instead of three (§9: node and datacenter documents are planned). If
a schema-described datacenter document is ever wanted, the natural form is a
`selector: {datacenter: true}` on a prefix, not a third ACL mapping.
