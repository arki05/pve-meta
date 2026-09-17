# 011 — The editor runs the core as wasm and reimplements no rule

**Status:** accepted. Mechanics in [`../WASM-CORE.md`](../WASM-CORE.md).

## Context

The editor held JavaScript copies of the codec (js-yaml), the key charset and other
rules the server already had, and every one of them drifted from the Rust at least
once: js-yaml's YAML 1.1 compatibility quoted values the store writes bare, so the diff
attributed the emitter's differences to whatever was being edited.

## Decision

`crates/pve-meta-wasm` is a plain `cargo build --target wasm32-unknown-unknown` of the
core behind a five-export JSON-string ABI, no wasm-bindgen, loaded lazily like Monaco.
The editor asks it for the codec and the document's rules rather than keeping a second
implementation. The server stays the authority: every answer the browser computes, the
server computes again on the real write.

## Consequences

The editor's YAML is the store's YAML by construction. The rules are tested in Rust;
`ui-extjs/testing/smoke.js` loads the built `.wasm` and tests the editor's own logic
over it.
