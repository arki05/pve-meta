# 024 — Access is PVE's ACLs

**Status:** accepted. Supersedes 001, 002 and 004.

## Context

pve-meta's own permission files let a token see and write one prefix of a document, on
the premise that a service should hold no more than it needs. No second consumer ever
needed it: pve-meta-traefik, the one token-holding consumer, already holds `VM.Audit`
on `/vms` — full read; pve-compose is the root CLI and checks nothing (016). The
blast-radius argument did not justify ~3,000 lines across every layer (a registry file
kind, a scope model, an editor Access column, wasm exports) for a consumer that does
not exist.

## Decision

Access is PVE's ACLs and nothing else (§4): `VM.Audit`/`VM.Config.Options` on a guest,
`Sys.Modify` on `/` for prefix files, root for the CLI. No pve-meta-specific permission
file, no scope, no per-prefix grant. If per-prefix grants are ever wanted, PVE ACL
paths — the regex hunk 004 recorded and declined — are the route, not a second
permission system.

## Consequences

`GET /meta/access` answers `{read, write}` for the whole document, not per prefix.
