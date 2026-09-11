# 006 — YAML on disk, read strictly, written canonically; JSON is a wire format

**Status:** accepted.

## Decision

Documents are YAML: readable by a human in `/etc/pve`, comment keys included. There is
no TOML and no format-preserving edit engine (both existed and were removed with the
code that needed them). JSON exists only as the `data` wire format of a view.

YAML is read strictly — anchors, aliases, explicit tags and complex keys are refused,
and the YAML 1.1 words `yes`/`no`/`on`/`off` stay strings — so the store's parser
decides what a document can hold and a client that wants a boolean writes `true`. A
document is written canonically from its value: block style, two-space indent, key
order kept. A free-form `#` comment survives only until something writes the file;
comment *keys* are data and survive anything.

## Consequences

The editor's YAML is the store's YAML by construction (011): one emitter, compiled to
wasm, and `testdata/yaml-cases.json` exists to notice when a `serde_yaml_ng` upgrade
moves the bytes. Booleans in `data` are `1`/`0` on the way out, the PVE convention
(017); `format=yaml` carries exact types for clients that need them.
