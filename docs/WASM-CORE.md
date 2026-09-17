# The core in the browser

The editor's rules — the YAML codec and the key charset — are `pve-meta-core`, compiled
for `wasm32-unknown-unknown` (`crates/pve-meta-wasm`) and asked through a hand-written
JSON-string ABI, so the editor reimplements none of them (`docs/decisions/011`). The
server stays the authority: every answer the browser computes, the server computes
again on the real write.

## The ABI

Five exports, no `wasm-bindgen`:

```
pm_alloc(len) -> ptr        pm_free(ptr, len)
pm_call(ptr, len) -> len    pm_output() -> ptr
pm_abi() -> u32
```

A request is `{"fn": name, "args": [...]}`; a response is `{"ok": value}` or
`{"err": {message, line?, column?}}`. `pm_abi()` returns `ABI`
(`crates/pve-meta-wasm/src/lib.rs`), checked by the JavaScript glue on attach, so a
`.wasm` and a script from different builds fail loudly rather than mis-read each other.
See that crate's module doc for the exact wire shape and what crosses.

## Building it

`make wasm` (a plain `cargo build --target wasm32-unknown-unknown --profile wasm`); the
toolchain needs the `wasm32-unknown-unknown` target installed (`docs/BUILD.md`). `make
build` and `make check` depend on it; `make install` ships the `.wasm` next to the
editor and fails if it is missing.
