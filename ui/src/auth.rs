//! CSRF/session bootstrap for same-origin (pveproxy-served) deployment.
//!
//! The UI is now a plain static file under `/pve2/js/pve-meta-ui/` with no per-request
//! server-side templating, so nothing injects a `window.Proxmox` global with a fresh
//! `CSRFPreventionToken` for us the way a templated page would (see
//! `docs/NATIVE-API-SPEC.md` "UI serving"). This module fills that gap itself:
//!
//! 1. If embedded in a same-origin parent frame (the PVE web UI's own tab iframe),
//!    copy `window.parent.PVE.CSRFPreventionToken` — cross-origin access throws, which
//!    surfaces as an `Err` from `js_sys::Reflect::get` (our "try/catch").
//! 2. Otherwise, if a `PVEAuthCookie` is present, renew it (`proxmox_login::Login`'s
//!    ticket-renewal: `POST /api2/json/access/ticket` with the userid from the ticket
//!    and the ticket string itself as the password) to mint a fresh token.
//!
//! Either way the resulting token is stored both on `window.Proxmox.CSRFPreventionToken`
//! and via `proxmox_yew_comp::store_csrf_token` (sessionStorage), so a subsequent
//! `authentication_from_cookie` call also succeeds on its own.

use js_sys::{Object, Reflect};
use wasm_bindgen::JsValue;

use proxmox_login::{Authentication, Login, TicketResult};
use proxmox_yew_comp::{ExistingProduct, ProjectInfo, get_cookie, store_csrf_token};

/// Resolve a usable [`Authentication`] for the current session, or `None` if there's no
/// usable ticket cookie at all (not logged in to PVE on this host).
pub async fn resolve_auth() -> Option<Authentication> {
    let ticket = extract_ticket_cookie()?;

    if let Some(token) = copy_parent_csrf_token() {
        remember_csrf_token(&token);
        let userid = ticket.userid().to_string();
        return Some(Authentication {
            api_url: String::new(),
            userid,
            ticket,
            clustername: None,
            csrfprevention_token: token,
        });
    }

    let auth = renew_ticket(ticket).await?;
    remember_csrf_token(&auth.csrfprevention_token);
    Some(auth)
}

/// Read the raw `PVEAuthCookie` value (if any) straight out of `document.cookie` and
/// parse it as a ticket. Mirrors `proxmox_yew_comp`'s private `extract_auth_from_cookie`
/// (not exported), minus the CSRF-token requirement.
fn extract_ticket_cookie() -> Option<proxmox_login::Ticket> {
    let project = ExistingProduct::PVE;
    let name = project.auth_cookie_name();
    let prefixes: Vec<String> = project
        .auth_cookie_prefixes()
        .iter()
        .map(|p| format!("{p}:"))
        .collect();

    for part in get_cookie().split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key != name {
            continue;
        }
        let Ok(decoded) = percent_encoding::percent_decode_str(value).decode_utf8() else {
            continue;
        };
        if !prefixes.iter().any(|p| decoded.starts_with(p.as_str())) {
            continue;
        }
        if let Ok(ticket) = decoded.parse() {
            return Some(ticket);
        }
    }
    None
}

/// `window.parent.PVE.CSRFPreventionToken`, if we're in a same-origin iframe and that
/// path resolves. `None` for a cross-origin parent (the property access throws) or a
/// non-embedded top-level window.
fn copy_parent_csrf_token() -> Option<String> {
    let window = web_sys::window()?;
    let parent = window.parent().ok()??;
    if JsValue::from(parent.clone()) == JsValue::from(window) {
        return None; // top-level window, no parent frame
    }
    let pve = Reflect::get(&parent, &JsValue::from_str("PVE")).ok()?;
    Reflect::get(&pve, &JsValue::from_str("CSRFPreventionToken"))
        .ok()?
        .as_string()
}

/// Renew `ticket` (`POST /api2/json/access/ticket`, userid + the ticket itself as the
/// password) to obtain a fresh `Authentication` with a current CSRF token.
async fn renew_ticket(ticket: proxmox_login::Ticket) -> Option<Authentication> {
    let login = Login::renew_ticket("", ticket);
    let request = login.request();

    let response = match gloo_net::http::Request::post(&request.url)
        .header("content-type", request.content_type)
        .body(request.body)
    {
        Ok(req) => req.send().await,
        Err(e) => {
            log::error!("pve-meta-ui: failed to build ticket-renewal request: {e}");
            return None;
        }
    };

    let response = match response {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("pve-meta-ui: ticket-renewal request failed: {e}");
            return None;
        }
    };

    if !response.ok() {
        log::error!(
            "pve-meta-ui: ticket renewal returned HTTP {}",
            response.status()
        );
        return None;
    }

    let text = match response.text().await {
        Ok(text) => text,
        Err(e) => {
            log::error!("pve-meta-ui: failed to read ticket-renewal response: {e}");
            return None;
        }
    };

    match login.response(&text) {
        Ok(TicketResult::Full(auth)) | Ok(TicketResult::HttpOnly(auth)) => Some(auth),
        Ok(_) => {
            log::error!("pve-meta-ui: ticket renewal unexpectedly asked for a second factor");
            None
        }
        Err(e) => {
            log::error!("pve-meta-ui: failed to parse ticket-renewal response: {e}");
            None
        }
    }
}

/// Store `token` both on `window.Proxmox.CSRFPreventionToken` (so any code that reads
/// the global directly sees it) and in sessionStorage (so
/// `authentication_from_cookie`'s own lookup succeeds afterwards).
fn remember_csrf_token(token: &str) {
    store_csrf_token(token);

    let Some(window) = web_sys::window() else {
        return;
    };
    let proxmox = get_or_create_object(&window, "Proxmox");
    let _ = Reflect::set(
        &proxmox,
        &JsValue::from_str("CSRFPreventionToken"),
        &JsValue::from_str(token),
    );
    let setup = get_or_create_object(&proxmox, "Setup");
    let _ = Reflect::set(
        &setup,
        &JsValue::from_str("auth_cookie_name"),
        &JsValue::from_str(ExistingProduct::PVE.auth_cookie_name()),
    );
}

/// `obj[key]` if it's already a non-null object, otherwise a freshly created (and
/// attached) one.
fn get_or_create_object(obj: &JsValue, key: &str) -> JsValue {
    let key = JsValue::from_str(key);
    if let Ok(existing) = Reflect::get(obj, &key) {
        if !existing.is_undefined() && !existing.is_null() {
            return existing;
        }
    }
    let created: JsValue = Object::new().into();
    let _ = Reflect::set(obj, &key, &created);
    created
}
