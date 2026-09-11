# 016 — The CLI writes, for root on the node, skipping only permissions

**Status:** accepted (2026-09-11).

## Context

`pve-meta get` existed so a hook script could read without a token or a running
pveproxy. The same script could not write, so Ansible, cloud-init and cron had to hold
an API token for something root on the node could already do with an editor.

## Decision

`set`, `merge` and `delete` call the same `api_put`/`api_delete` the REST module calls,
with a root ACL carrying the guest's tags, under the same per-document cluster lock
with the guest's existence re-checked inside it. Lint, digest compare-and-swap,
enforced schemas (`--force`) and the audit line all apply. Permissions are the one
thing skipped, because root can already write the file. `--file -` reads stdin.

## Consequences

Two callers of the write path, one implementation. `rm` (009) is the same shape for
orphans.
