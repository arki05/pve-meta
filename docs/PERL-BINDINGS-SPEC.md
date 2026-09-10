# libpve-meta-rs-perl — `PVE::RS::Meta`

Superseded. The behaviour behind the bindings is [`DESIGN.md`](DESIGN.md): the
Perl/Rust boundary and its one string crossing (§5, "Implementation"), the lifecycle
hooks and the manual GC (§6), the API layer they wrap (§4, §5). The exports themselves
— each function's arguments, return values and the errors it dies with — are documented
where they are defined, in `crates/pve-meta-perl/src/lib.rs`; the two-phase GC shape a
caller must use, with the three things in it that are not optional, is the header of
`libexec/gc`. Build and packaging are `crates/pve-meta-perl/PACKAGING.md` and
`docs/BUILD.md`.

This file used to restate all of that and had drifted from the code — a helper that
never existed, a field renamed, an export missing, a test described as asserting the
rule a revision ago replaced. It is kept only as a redirect for old links; do not add
content here.
