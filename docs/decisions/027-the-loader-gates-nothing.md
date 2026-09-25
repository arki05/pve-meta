# 027 — The page loader gates nothing, and every page is a script

**Status:** accepted (473199f).

## Context

pve-ext's page loader had two mechanisms nothing used. A manifest could name the
capabilities a viewer needed (`requires`) and the loader hid the tab from anyone
without them — a permission check in the browser, in a cluster with one administrator,
for tabs whose backends check the same thing again and are the only place it counts. A
manifest could also describe an iframe page instead of a native ExtJS class, a second
page form with its own placeholder substitution, sizing and theme plumbing; every page
written against it is the script form.

## Decision

A page manifest is checked for the fields the loader needs (`id`, `title`, `targets`,
`script`, `xtype`) and skipped with a warning otherwise. There is no `requires` and no
iframe form: the tab is offered to everyone who can load the UI, and a page's own
backend API owns its access control, in its `permissions`, like any other PVE endpoint.

## Consequences

A user without the rights sees a tab whose content reports the 403 its backend
returned, instead of not seeing the tab — the honest failure, and the one PVE's own UI
already produces. Anything the iframe form carried (a non-ExtJS page, a foreign origin)
would have to be reintroduced deliberately; nothing wants it. The same commit dropped
the patch tool's claim identity, for the same reason: it guarded a collision two
disjoint manifests cannot have.
