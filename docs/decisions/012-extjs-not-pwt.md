# 012 — The editor is plain ExtJS, not pwt/Yew

**Status:** accepted. The pwt implementation was removed at git tag `pwt-ui-removed`.

## Context

The direction document argued for pwt on the premise that `Ext.tree.Panel` had no pwt
equivalent. It does (`DataTable` + `TreeStore`, used by PDM). Both were built to the
same specification and compared on the lab.

## Decision

ExtJS. It won on size and build surface — about 2,200 lines of JavaScript against about
4,900 lines of Rust and 259 crates at the time — and on integration: a native panel
mounted through pve-ext's `script`+`xtype` manifest gets session, CSRF, theme and i18n
from the PVE UI with no iframe and no build step. pwt's one structural advantage, never
parsing YAML itself, was taken by 011 instead.

## Consequences

One 5,000-line file that follows pve-manager's ExtJS. The design notes from the
evaluation are under `design/`; its screenshots were not kept.
