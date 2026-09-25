# 029 — pve-meta writes nothing into guests

**Status:** accepted; supersedes 023. `pve-meta-guest-files` is deprecated in 0.3.3 and
removed in 0.4, together with 023.

## Context

023 made publishing into containers a separate package, and it became
`pve-meta-guest-files`: a daemon on every node that renders views of a container's
document into files inside it, with a managed plane for operators on top. It was a
bootstrap convenience, so a service could read its configuration before anything spoke
the API. It also made a document write an effect on a guest — root inside it, since the
entry picks path, owner and mode — turning `VM.Config.Options`, a routine per-guest
privilege, into something no PVE role grants. pve-compose, its only operator, is moving
to `pct push`.

## Decision

pve-meta has no effects on guests. Structured metadata is stored and served; nothing of
pve-meta writes it into a container. A consumer is an API client with explicit, scoped
permissions — say a read-only config-sync service inside a container, with a token that
reads its own document and updates its own file. Anything that does manage files in a
guest, a container file editor for one, is a separate plugin that does so itself, under
its own permissions. The whole of guest-files goes, managed plane included; it gains
nothing new before it does.

## Consequences

A document write reaches a guest only through something the administrator installed
and granted on its own. Until 0.4 the package keeps working as `GUEST-FILES.md`
specifies and logs a deprecation warning when its daemon starts; the `guest-files` prefix
and its files in containers stay where they are, and whoever used it moves to a scoped
token or to `pct push` before upgrading.
