# 026 — The editor writes each edit

**Status:** accepted. Supersedes 010.

## Context

010 staged row edits into an edit set so the tree could plan multi-key changes (a
selector's `all`/`tag` swap) before one Apply. Staging cost a second model kept in step
with the stored document, a subsumption rule, and bugs where the staged view and the
real document disagreed after a 409. PVE's own editors — Notes, firewall rules — write
each change as it is made and reload on conflict; nothing else in the platform stages.

## Decision

A row edit is one write: `set` is `view::replace`, `delete` is `view::remove`, sent
immediately with the digest the tree last loaded, as everywhere else in PVE (§8). A 409
reloads. The digest already guards concurrent edits; there is nothing left for staging
to buy.

## Consequences

No edit set, no "Save anyway" queue of unrelated changes, no poll held off while
something is staged. A change that genuinely needs several keys atomically (the
selector swap) is one `PUT` with a multi-key `data` body, planned and applied together
exactly as an API caller would.
