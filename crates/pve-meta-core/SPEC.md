# pve-meta-core

Superseded. What this crate does is specified by [`docs/DESIGN.md`](../../docs/DESIGN.md)
— the document model and views (§2), prefixes and permissions and how a request's
access is resolved (§3), documents on the wire and the store's caps (§4), the API layer
(§5), the GC (§6), and `Shape` and `EditSet`, the two types the editor asks through the
wasm build (§8). What the crate *is* — the module map, every public signature, every
constant and its value, the invariants each module keeps — is its own rustdoc: start at
`src/lib.rs` and read `make doc` (`cargo doc --no-deps -p pve-meta-core`), which fails
on a broken link so the map cannot rot silently.

This file used to restate both and had drifted from each — a type that no longer
exists, a module that never made it in, an authorization rule from a previous
revision. It is kept only as a redirect for old links; do not add content here.
