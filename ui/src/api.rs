//! Typed wrappers over the `PVE::API2::Meta` HTTP API (`docs/API.md`,
//! `docs/NATIVE-API-SPEC.md`), served natively by pveproxy/pvedaemon at
//! `/api2/json/meta/...` on the same origin as the PVE web UI.
//!
//! This module talks to `fetch` directly (via `gloo-net`) rather than going through
//! `proxmox_yew_comp::http_get/http_put/http_post`, for two reasons specific to the
//! native module:
//!
//! * PVE request parameters are form/JSON parameters where **object-valued parameters
//!   are JSON-encoded strings** — the `patch` parameter of the patch endpoint is a JSON
//!   string, not a nested JSON object — which `http_put`'s plain `serde_json::to_value`
//!   body encoding doesn't do.
//! * Error classification (409/400/404) must be based on the actual HTTP status code.
//!   `proxmox_yew_comp`'s `http_*` helpers funnel every response through
//!   `proxmox_client`'s `RawApiResponse`, which derives the status from an (optional,
//!   defaulting to 400) `status` field *inside the JSON body* — not the transport-level
//!   status pveproxy actually replies with (`{"data": null, "message": "...", "errors":
//!   {...}}`, per `docs/NATIVE-API-SPEC.md`). Reading `Response::status()` directly
//!   avoids that mismatch.
//!
//! The CSRF token is read fresh on every mutating call via
//! `proxmox_yew_comp::http_get_auth()`, so it stays correct across `crate::auth`'s
//! bootstrap and `proxmox_yew_comp`'s own background ticket-refresh loop.

use std::collections::HashMap;
use std::fmt;

use anyhow::{anyhow, Error};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use proxmox_yew_comp::{http_get_auth, json_object_to_query};

use crate::model::{DocId, Document};

/// One entry of `GET /meta/inventory`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct InventoryEntry {
    pub vmid: u32,
    pub node: String,
    #[serde(rename = "type")]
    pub guest_type: String,
    #[serde(default)]
    pub name: Option<String>,
    // PVE's `type => 'boolean'` schema fields are commonly rendered as a plain 0/1
    // integer on the wire, not a JSON `true`/`false` (observed live against
    // `PVE::API2::Meta`) — `proxmox_serde::perl::deserialize_bool` accepts either.
    #[serde(default, deserialize_with = "proxmox_serde::perl::deserialize_bool")]
    pub has_meta: bool,
    #[serde(default)]
    pub format: Option<String>,
}

/// One claim of an operator in the registry.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Claim {
    pub prefix: String,
    pub scope: String,
}

/// One entry of `GET /meta/registry`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Operator {
    pub name: String,
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub schemas: HashMap<String, Value>,
    #[serde(default)]
    pub description: Option<String>,
}

impl Operator {
    /// The claim covering `namespace` (an exact top-level-key match), if any.
    pub fn claim_for<'a>(&'a self, namespace: &str) -> Option<&'a Claim> {
        self.claims.iter().find(|c| c.prefix == namespace)
    }
}

/// Find the first operator claiming `namespace`, and the scope of that claim.
pub fn find_owner<'a>(registry: &'a [Operator], namespace: &str) -> Option<(&'a Operator, &'a str)> {
    registry
        .iter()
        .find_map(|op| op.claim_for(namespace).map(|c| (op, c.scope.as_str())))
}

/// `GET /meta/version` response.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VersionInfo {
    pub token: String,
    #[serde(default)]
    pub changed: i64,
}

pub async fn inventory() -> Result<Vec<InventoryEntry>, Error> {
    get_json("/meta/inventory", None).await
}

pub async fn registry() -> Result<Vec<Operator>, Error> {
    get_json("/meta/registry", None).await
}

/// The JSON schemas applicable to a guest document, keyed by namespace prefix. There is
/// no equivalent endpoint for the datacenter document.
pub async fn schemas(vmid: u32) -> Result<HashMap<String, Value>, Error> {
    get_json(&format!("/meta/schemas/{vmid}"), None).await
}

/// Fetch a document. `comments` keeps `key__`/`__` comment keys in `data`; `raw` also
/// fetches the file text.
pub async fn get(id: DocId, comments: bool, raw: bool) -> Result<Document, Error> {
    let mut params = Map::new();
    if comments {
        params.insert("comments".into(), json!(1));
    }
    if raw {
        params.insert("raw".into(), json!(1));
    }
    let query = if params.is_empty() {
        None
    } else {
        Some(Value::Object(params))
    };
    get_json(&id.api_path(), query).await
}

/// Apply a merge patch. `digest` pins optimistic concurrency (a `409` means the
/// document changed on the server since it was loaded); `dry_run` validates and returns
/// the would-be result without writing.
pub async fn patch(
    id: DocId,
    patch: Value,
    digest: Option<&str>,
    dry_run: bool,
) -> Result<Document, Error> {
    let mut body = Map::new();
    // Object-valued PVE parameters are JSON-encoded strings, not nested JSON.
    body.insert("patch".into(), json!(serde_json::to_string(&patch)?));
    if let Some(d) = digest {
        body.insert("digest".into(), json!(d));
    }
    if dry_run {
        body.insert("dry_run".into(), json!(1));
    }
    send_json("PUT", &id.api_path(), Value::Object(body)).await
}

/// Full text replace (`PUT .../raw`). `format` switches the file extension/format.
pub async fn put_raw(
    id: DocId,
    content: String,
    format: Option<&str>,
    digest: Option<&str>,
    dry_run: bool,
) -> Result<Document, Error> {
    let mut body = Map::new();
    body.insert("content".into(), json!(content));
    if let Some(f) = format {
        body.insert("format".into(), json!(f));
    }
    if let Some(d) = digest {
        body.insert("digest".into(), json!(d));
    }
    if dry_run {
        body.insert("dry_run".into(), json!(1));
    }
    send_json("PUT", &format!("{}/raw", id.api_path()), Value::Object(body)).await
}

/// Re-dump the document in a new format (`POST .../convert`).
pub async fn convert(id: DocId, format: &str, digest: Option<&str>) -> Result<Document, Error> {
    let mut body = Map::new();
    body.insert("format".into(), json!(format));
    if let Some(d) = digest {
        body.insert("digest".into(), json!(d));
    }
    send_json("POST", &format!("{}/convert", id.api_path()), Value::Object(body)).await
}

/// `GET /meta/version`. No long-poll on the native module (`docs/NATIVE-API-SPEC.md`):
/// it answers immediately, `wait`/`since` are accepted but ignored — poll this on an
/// interval instead (`crate::app` does so every 5s).
pub async fn version() -> Result<VersionInfo, Error> {
    get_json("/meta/version", None).await
}

/// An API error carrying the actual HTTP status code (not a body-embedded one — see
/// this module's doc comment).
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.message, self.status)
    }
}

impl std::error::Error for ApiError {}

/// True if `err` is an HTTP 409 (digest conflict: the document changed on the server).
pub fn is_conflict(err: &Error) -> bool {
    status_of(err) == Some(409)
}

/// True if `err` is an HTTP 400 (lint/parse error from the daemon).
pub fn is_bad_request(err: &Error) -> bool {
    status_of(err) == Some(400)
}

/// True if `err` is an HTTP 404 (no document yet for this guest/datacenter — writing
/// one creates it, per `docs/API.md`).
pub fn is_not_found(err: &Error) -> bool {
    status_of(err) == Some(404)
}

fn status_of(err: &Error) -> Option<u16> {
    err.downcast_ref::<ApiError>().map(|e| e.status)
}

/// A user-facing message for any error from an API call.
pub fn error_text(err: &Error) -> String {
    match err.downcast_ref::<ApiError>() {
        Some(e) => e.to_string(),
        None => err.to_string(),
    }
}

/// `Some(digest)` unless it's empty (an empty digest means "no document yet" —
/// see [`crate::model::Document::empty`] — and must not be sent as an expected digest).
pub fn digest_opt(digest: &str) -> Option<&str> {
    if digest.is_empty() {
        None
    } else {
        Some(digest)
    }
}

/// The `{"data": ..., "message": "...", "errors": {...}}` envelope every PVE API
/// response (success or failure) is wrapped in.
#[derive(Deserialize)]
struct Envelope<T> {
    // Note: no `#[serde(default)]` here — that would make serde-derive require
    // `T: Default` for the whole struct (it doesn't see through `Option<T>`'s own
    // blanket `Default` impl). PVE's envelope always includes `"data"`, `null` on
    // failure, which plain `Option<T>` already deserializes as `None` on its own.
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
