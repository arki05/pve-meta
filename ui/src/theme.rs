//! Bridging PVE's color theme into pwt's theme state.
//!
//! PVE resolves its theme server-side: `Proxmox.window.ThemeEditWindow` writes a
//! `PVEThemeCookie` cookie and reloads, and pveproxy picks the stylesheet when it renders
//! the templated `index.html`. This page is a *static* file under
//! `/pve2/js/pve-meta-ui/`, so it never goes through that template and would otherwise
//! stay light while the surrounding SPA is dark. pwt, meanwhile, keeps
//! `ThemeName`/`ThemeMode` in `localStorage`.
//!
//! So the mapping has to be written before the first render (see
//! `docs/design/PDM-DESIGN-LANGUAGE.md` §12.3): `crisp` → light, `proxmox-dark` → dark,
//! absent or `__default__` → follow the OS. PVE is the master while embedded; the page
//! has no theme switcher of its own.

use pwt::state::{Theme, ThemeMode};

use proxmox_yew_comp::get_cookie;

/// The pwt theme this page always uses. Crisp is the one designed to look like the
/// Proxmox products (3px spacing, the small font scale, the ExtJS accent blue).
pub const THEME_NAME: &str = "Crisp";

/// Write `ThemeName`/`ThemeMode` from `?theme=` (as substituted by `pve-ext-loader.js`)
/// or, failing that, from `PVEThemeCookie`.
pub fn bridge(theme_param: Option<&str>) {
    if let Err(err) = Theme::store_theme_name(THEME_NAME) {
        log::error!("pve-meta-ui: failed to store theme name: {err}");
    }

    let mode = match theme_param {
        Some("dark") => ThemeMode::Dark,
        Some("light") => ThemeMode::Light,
        _ => mode_from_cookie(),
    };

    if let Err(err) = Theme::store_theme_mode(mode) {
        log::error!("pve-meta-ui: failed to store theme mode: {err}");
    }
}

/// `PVEThemeCookie`, the same way `pve-ext-loader.js` reads it.
fn mode_from_cookie() -> ThemeMode {
    for part in get_cookie().split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key != "PVEThemeCookie" {
            continue;
        }
        return match value {
            "proxmox-dark" => ThemeMode::Dark,
            "crisp" => ThemeMode::Light,
            _ => ThemeMode::System,
        };
    }
    ThemeMode::System
}
