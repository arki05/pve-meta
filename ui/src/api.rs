//! Typed wrappers over the native metadata API (`docs/DESIGN.md` §3), served by
//! pveproxy/pvedaemon at `/api2/json/meta/...` on the same origin as the PVE web UI.
//!
//! This module talks to `fetch` (via `gloo-net`) rather than going through
//! `proxmox_yew_comp::http_get`/`http_put`, because error classification (409 digest
//! conflict, 403 out-of-scope write) must follow the actual HTTP status code.
//! `proxmox_yew_comp`'s helpers funnel every response through `proxmox_client`'s
//! `RawApiResponse`, which derives the status from an optional `status` field *inside*
//! the JSON body — not the transport-level status pveproxy replies with.
//!
//! The CSRF token is read fresh on every mutating call via
//! `proxmox_yew_comp::http_get_auth()`, so it stays correct across `crate::auth`'s
//! bootstrap and `proxmox_yew_comp`'s own background ticket refresh.

use std::fmt;

use anyhow::{Error, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use proxmox_yew_comp::{http_get_auth, json_object_to_query};

use crate::model::{Access, DocId};

/// One entry of `GET /meta/guests`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GuestEntry {
    pub vmid: u32,
    #[serde(default)]
    pub node: String,
    #[serde(default, rename = "type")]
    pub guest_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// A document view rendered as text (`format=yaml`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DocText {
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub text: String,
}

/// A document view as data (`format=json`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DocData {
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub data: Value,
}

/// The result of a write.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct WriteResult {
    #[serde(default)]
    pub digest: String,
    /// The paths the write touched. Kept opaque on purpose: `docs/DESIGN.md` §3 spells
    /// this as a list of paths while the API module answers `{op, path}` objects, and
    /// the page displays neither — it reloads instead.
    #[serde(default)]
    pub touched: Vec<Value>,
}

/// `GET /meta/version` — a content hash over the store, to be polled.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct VersionInfo {
    #[serde(default)]
    pub token: String,
}

/// The caller's effective grants (`GET /meta/access`).
pub async fn access() -> Result<Access, Error> {
    get_json("/meta/access", None).await
}

/// Every guest in the vmlist, with the top-level keys visible to the caller.
pub async fn guests() -> Result<Vec<GuestEntry>, Error> {
    get_json("/meta/guests", None).await
}

/// A view of a document as YAML text. An empty `view` is the whole document.
pub async fn get_text(doc: DocId, view: &str) -> Result<DocText, Error> {
    let mut query = Map::new();
    query.insert("format".into(), json!("yaml"));
    if !view.is_empty() {
        query.insert("view".into(), json!(view));
    }
    get_json(&doc.api_path(), Some(Value::Object(query))).await
}

/// The whole document as data — the "View as" selector needs its top-level keys.
pub async fn get_data(doc: DocId) -> Result<DocData, Error> {
    get_json(&doc.api_path(), Some(json!({ "format": "json" }))).await
}

/// Replace `view` of `doc` with `text`. `digest` pins optimistic concurrency: a 409
/// means the document changed on the server since it was loaded.
pub async fn put_text(
    doc: DocId,
    view: &str,
    text: String,
    digest: &str,
) -> Result<WriteResult, Error> {
    let mut body = Map::new();
    // No `format` here: the write endpoint infers it from which payload parameter is
    // sent — `text` is YAML, `data` is JSON — and rejects a `format` key outright.
    body.insert("mode".into(), json!("replace"));
    body.insert("text".into(), json!(text));
    if !view.is_empty() {
        body.insert("view".into(), json!(view));
    }
    // An empty digest means "no document yet" and must not be sent as an expected one.
    if !digest.is_empty() {
        body.insert("digest".into(), json!(digest));
    }
    send_json("PUT", &doc.api_path(), Value::Object(body)).await
}

/// `GET /meta/version`. The native module answers immediately (no long poll), so this is
/// polled on an interval.
pub async fn version() -> Result<VersionInfo, Error> {
    get_json("/meta/version", None).await
}

/// An API error carrying the actual HTTP status code (not a body-embedded one).
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The server message verbatim — `docs/DESIGN.md` §6 asks for exactly that.
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// True if `err` is an HTTP 409: the document changed on the server since it was loaded.
pub fn is_conflict(err: &Error) -> bool {
    status_of(err) == Some(409)
}

fn status_of(err: &Error) -> Option<u16> {
    err.downcast_ref::<ApiError>().map(|e| e.status)
}

/// The `{"data": ..., "message": "..."}` envelope every PVE API response is wrapped in.
#[derive(Deserialize)]
struct Envelope<T> {
    // No `#[serde(default)]`: that would make serde-derive require `T: Default` for the
    // whole struct. PVE's envelope always includes `"data"`, `null` on failure, which
    // plain `Option<T>` already deserializes as `None`.
    data: Option<T>,
    #[serde(default)]
    message: Option<String>,
}

async fn get_json<T: DeserializeOwned>(path: &str, query: Option<Value>) -> Result<T, Error> {
    let mut url = format!("/api2/json{path}");
    if let Some(query) = query {
        let qs = json_object_to_query(query)?;
        if !qs.is_empty() {
            url.push('?');
            url.push_str(&qs);
        }
    }

    let request = gloo_net::http::Request::get(&url)
        .build()
        .map_err(|e| anyhow!("failed to build request: {e}"))?;

    send(request).await
}

async fn send_json<T: DeserializeOwned>(method: &str, path: &str, body: Value) -> Result<T, Error> {
    let url = format!("/api2/json{path}");
    let mut builder = match method {
        "PUT" => gloo_net::http::Request::put(&url),
        "POST" => gloo_net::http::Request::post(&url),
        "DELETE" => gloo_net::http::Request::delete(&url),
        _ => return Err(anyhow!("unsupported method {method}")),
    };

    if let Some(auth) = http_get_auth() {
        builder = builder.header("CSRFPreventionToken", &auth.csrfprevention_token);
    } else {
        log::warn!("pve-meta-ui: sending a {method} request with no known CSRF token");
    }

    let request = builder
        .header("content-type", "application/json")
        .body(serde_json::to_string(&body)?)
        .map_err(|e| anyhow!("failed to build request: {e}"))?;

    send(request).await
}

async fn send<T: DeserializeOwned>(request: gloo_net::http::Request) -> Result<T, Error> {
    let response = request
        .send()
        .await
        .map_err(|e| anyhow!("network error: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| anyhow!("failed to read response body: {e}"))?;

    if (200..300).contains(&status) {
        let envelope: Envelope<T> =
            serde_json::from_str(&text).map_err(|e| anyhow!("failed to parse response: {e}"))?;
        envelope
            .data
            .ok_or_else(|| anyhow!("response carried no data"))
    } else {
        let message = serde_json::from_str::<Envelope<Value>>(&text)
            .ok()
            .and_then(|e| e.message)
            .unwrap_or(text);
        Err(ApiError { status, message }.into())
    }
}
