# 016 — The CLI writes, for root on the node, checking nothing

**Status:** accepted.

## Context

`pve-meta get` existed so a hook script could read without a token or a running
pveproxy. The same script could not write, so Ansible, cloud-init and cron had to hold
an API token for something root on the node could already do with an editor.

## Decision

`set`, `merge` and `delete` call the same functions the REST module calls, under the
same per-document cluster lock with the guest's existence re-checked inside it. Lint,
digest compare-and-swap, enforced schemas (`--force`) and the audit line all apply. No
access check runs, because root can already write the file (§4). `--file -` reads
stdin.

## Consequences

Two callers of the write path, one implementation. `rm` (009) is the same shape for
orphans.
