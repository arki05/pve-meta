//! pve-meta editor UI — wasm entry point.
//!
//! Everything else lives in the `pve_meta_ui` lib crate (`src/lib.rs`); this binary is
//! only the bootstrap, so a native `cargo build`/`cargo test` of the workspace doesn't
//! need to touch any wasm-only dependency (see `src/lib.rs`'s doc comment).

#[cfg(target_arch = "wasm32")]
fn main() {
    use pwt::state::{Theme, ThemeMode};
    use proxmox_yew_comp::{http_setup, ExistingProduct};

    wasm_logger::init(wasm_logger::Config::default());

    // Theme-from-query-param, before the first render (see UI-SPEC.md "Embedded
    // mode / theme"): `?theme=dark|light` forces the mode; anything else (including
    // absent) leaves it at `System` (follow the OS/browser preference).
    if let Some(win) = web_sys::window() {
        if let Ok(search) = win.location().search() {
            for (k, v) in url::form_urlencoded::parse(search.trim_start_matches('?').as_bytes()) {
                if k == "theme" {
                    let mode = match v.as_ref() {
                        "dark" => ThemeMode::Dark,
                        "light" => ThemeMode::Light,
                        _ => ThemeMode::System,
                    };
                    if let Err(e) = Theme::store_theme_mode(mode) {
                        log::error!("pve-meta-ui: failed to store theme mode: {e}");
                    }
                }
            }
        }
    }

    http_setup(&ExistingProduct::PVE);
    pwt::state::set_available_themes(&["Desktop", "Crisp"]);

    // `DesktopApp` wraps its body in a `CatalogLoader`, which — even for the default
    // ("en", no catalog fetched) language — calls `pwt::state::get_language_info`
    // once loading "finishes"; that panics ("cannot access available languages
    // before they've been set") unless `set_available_languages` was called first.
    // We don't otherwise use i18n (no `.catalog_url_builder` is set, so only English
    // is ever loaded), but pwt still needs the list to exist. Same call PDM's
    // `ui/src/main.rs` makes before rendering.
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
