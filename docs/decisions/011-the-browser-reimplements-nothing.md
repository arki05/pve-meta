# 011 — The editor runs the core as wasm and reimplements no rule

**Status:** accepted (2026-09-10). Mechanics in [`../WASM-CORE.md`](../WASM-CORE.md).

## Context

The editor held JavaScript copies of the codec (js-yaml), the key charset, the coverage
rule, the governing-prefix rule and the schema walk. Every one of them drifted from the
Rust at least once: js-yaml's YAML 1.1 compatibility quoted values the store writes
bare and indented sequences the store writes flush, so the diff attributed the emitter's
differences to whatever was being edited; two shared JSON case tables existed only to
keep two implementations honest.

## Decision

`crates/pve-meta-wasm` is a plain `cargo build --target wasm32-unknown-unknown` of the
core behind a five-export JSON-string ABI, no wasm-bindgen, loaded lazily like Monaco.
The editor asks it for the codec, the key rules, `scopes::Effective`, `shape::Shape` and
`edit::EditSet`. What stays JavaScript is presentation: rows, marker placement, hover
text, and the `format:` check, which the core hands back to be run through proxmoxlib's
own vtype rather than a third implementation of what `ipv4` means.

The server stays the authority: every answer the browser computes, the server computes
again on the real write.

## Consequences

The editor's YAML is the store's YAML by construction. The rules are tested in Rust;
`ui-extjs/testing/smoke.js` loads the built `.wasm` and tests the editor's own logic
over it. The core-backed helpers fail closed before the wasm loads, except the two name
validators, which fail open because the server refuses the same names anyway.
