//! Typed wrappers over `proxmox_yew_comp::http_get/http_put/http_post` for the
//! `pve-metad` HTTP API (`docs/API.md`).
//!
//! These helpers go through the `/api2/extjs` prefix (baked into
//! `proxmox_yew_comp::http_*`), which `docs/API.md`/`UI-SPEC.md` explicitly call out as
//! fine: the daemon serves the same handlers under both `/api2/json` and `/api2/extjs`.

use std::collections::HashMap;

use anyhow::Error;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use proxmox_yew_comp::{http_get, http_post, http_put};

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
    #[serde(default)]
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
    http_get("/meta/inventory", None).await
}

pub async fn registry() -> Result<Vec<Operator>, Error> {
    http_get("/meta/registry", None).await
}

/// The JSON schemas applicable to a guest document, keyed by namespace prefix. There is
/// no equivalent endpoint for the datacenter document.
pub async fn schemas(vmid: u32) -> Result<HashMap<String, Value>, Error> {
    http_get(format!("/meta/schemas/{vmid}"), None).await
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
    let data = if params.is_empty() {
        None
    } else {
        Some(Value::Object(params))
    };
    http_get(id.api_path(), data).await
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
    body.insert("patch".into(), patch);
    if let Some(d) = digest {
        body.insert("digest".into(), json!(d));
    }
    if dry_run {
        body.insert("dry_run".into(), json!(true));
    }
    http_put(id.api_path(), Some(Value::Object(body))).await
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
        body.insert("dry_run".into(), json!(true));
    }
    http_put(format!("{}/raw", id.api_path()), Some(Value::Object(body))).await
}

/// Re-dump the document in a new format (`POST .../convert`).
pub async fn convert(id: DocId, format: &str, digest: Option<&str>) -> Result<Document, Error> {
    let mut body = Map::new();
    body.insert("format".into(), json!(format));
    if let Some(d) = digest {
        body.insert("digest".into(), json!(d));
    }
    http_post(format!("{}/convert", id.api_path()), Some(Value::Object(body))).await
}

/// Long-poll `GET /meta/version`. `wait` is capped at 60s server-side; `since` is the
/// last known token (omit on the very first call).
pub async fn version(wait: Option<u32>, since: Option<&str>) -> Result<VersionInfo, Error> {
    let mut params = Map::new();
    if let Some(w) = wait {
        params.insert("wait".into(), json!(w));
    }
    if let Some(s) = since {
        params.insert("since".into(), json!(s));
    }
    let data = if params.is_empty() {
        None
    } else {
        Some(Value::Object(params))
    };
    http_get("/meta/version", data).await
}

/// The HTTP status code of an API error, if it is one (as opposed to e.g. a network or
/// deserialization failure).
fn status_of(err: &Error) -> Option<u16> {
    err.downcast_ref::<proxmox_client::Error>()
        .and_then(|e| match e {
            proxmox_client::Error::Api(status, _) => Some(status.as_u16()),
            _ => None,
        })
}

/// True if `err` is an HTTP 409 (digest conflict: the document changed on the server).
pub fn is_conflict(err: &Error) -> bool {
    status_of(err) == Some(409)
}

/// True if `err` is an HTTP 400 (lint/parse error from the daemon).
pub fn is_bad_request(err: &Error) -> bool {
    status_of(err) == Some(400)
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

/// True if `err` is an HTTP 404 (no document yet for this guest/datacenter — writing
/// one creates it, per `docs/API.md`).
pub fn is_not_found(err: &Error) -> bool {
    status_of(err) == Some(404)
}

/// A user-facing message for any error from an API call.
pub fn error_text(err: &Error) -> String {
    match err.downcast_ref::<proxmox_client::Error>() {
        Some(proxmox_client::Error::Api(status, msg)) => format!("{msg} (HTTP {status})"),
        _ => err.to_string(),
    }
}
