# 023 — Publishing into containers is a separate package that pushes files one way

**Status:** superseded by [029](029-pve-meta-writes-nothing-into-guests.md): the package
is deprecated in 0.3.3, and this record goes with it in 0.4.

## Context

The most common consumer of a document is a service inside the guest it describes.
pve-meta runs nothing on its own (no sweeper, 009), but a stopped container must be
written when it starts, and a file edited in the guest is seen only by looking.

## Decision

A third binary package, `pve-meta-publish`, pinned to `pve-meta`'s exact version. A
daemon on every node reads each container's `publish` key and writes the views it names
with `pct exec`, on a digest change, on container start, and every ten minutes for
drift — one way, host to guest. Its manifest records what was written by hash; a file
no longer matching its record is locally edited and kept unless the entry says
`overwrite`. Paths may be absolute: `publish` is already root, so confinement buys no
security. Rejected: inside `pve-meta`; the write path; a bind mount; a guest-side
service; two-way sync.

## Consequences

Whoever may write a guest's document can place any file, owner and mode, at any
admitted path. A daemon killed mid-write leaves a file looking edited, never lost.
