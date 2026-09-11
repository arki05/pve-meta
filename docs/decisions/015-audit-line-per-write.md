# 015 — Every write logs one audit line

**Status:** accepted (2026-09-11).

## Context

Nothing recorded who changed metadata. Writes are plain API calls, not tasks, so PVE's
task log never sees them.

## Decision

`put_document` and `delete_document` log one syslog line at `info` for every write that
changed a file, tagged `pve-meta audit:`, with the authid, document id, view, mode,
touched count and new digest prefix. Dry runs and writes that put back the bytes
already on disk log nothing; reads never do. syslog only: the daemons have no other
route out (`PVE::Daemon` dups stderr to `/dev/null`), and the CLI prints its own result.

## Consequences

`journalctl | grep 'pve-meta audit'` is the history of the store. The line inherits
whatever syslog ident the host process last set, which is why it carries its own tag.
