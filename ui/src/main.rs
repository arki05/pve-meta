//! pve-meta editor UI — wasm entry point.
//!
//! Everything else lives in the `pve_meta_ui` lib crate (`src/lib.rs`); this binary is
//! only the bootstrap, so a native `cargo build`/`cargo test` of the crate doesn't need
//! to touch any wasm-only dependency (see `src/lib.rs`'s doc comment).

#[cfg(target_arch = "wasm32")]
fn main() {
    use proxmox_yew_comp::{ExistingProduct, http_setup};

    wasm_logger::init(wasm_logger::Config::default());

    // Crisp first: it is the default theme, and the one written to look like the
    // Proxmox products. Desktop stays available for a standalone browse.
    pwt::state::set_available_themes(&["Crisp", "Desktop"]);

    // PVE resolves its theme server-side and this page is a static file, so the mapping
    // has to be written into pwt's own state before the first render.
    let query = pve_meta_ui::app::location_query();
    pve_meta_ui::theme::bridge(query.get("theme").map(String::as_str));

    http_setup(&ExistingProduct::PVE);

    // `DesktopApp` wraps its body in a `CatalogLoader`, which — even for the default
    // ("en", no catalog fetched) language — calls `pwt::state::get_language_info` once
    // loading "finishes"; that panics ("cannot access available languages before
    // they've been set") unless `set_available_languages` was called first. We set no
    // `catalog_url_builder`, so only English is ever loaded, but pwt still needs the
    // list to exist. Same call PDM's `ui/src/main.rs` makes before rendering.
    pwt::state::set_available_languages(proxmox_yew_comp::available_language_list());

    yew::Renderer::<pve_meta_ui::app::App>::new().render();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!(
        "pve-meta-ui is a wasm32 web app; build it with `trunk build` on the Linux build \
         host (see ui/README.md), not `cargo build` on the Mac."
    );
}
