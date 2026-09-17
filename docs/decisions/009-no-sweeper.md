# 009 — No garbage collector: hooks on create and destroy, a CLI for orphans

**Status:** accepted. Supersedes the hourly sweep and the manual broom.

## Context

A periodic sweep nominated vmids missing from the vmlist, but could not see a guest
destroyed and recreated at the same vmid between two runs, so the new guest inherited
the old document for good. Once `on_create` and `on_destroy` existed (008), the sweep's
only remaining job was a guest config removed out of band — for that it carried a
libexec script, three Perl exports and a two-phase lock design.

## Decision

No sweeper, on a timer or by hand. `pve-meta ls --orphans` lists the vmids the store
holds files for that are not in the vmlist. `pve-meta rm <vmid>` removes one under the
document's own write lock with the vmlist re-read inside it, refuses a live guest, and
refuses an empty vmlist.

## Consequences

An orphan is something an administrator sees and removes, with no separate locking
story to get wrong. There is no orphan concept in the API.
