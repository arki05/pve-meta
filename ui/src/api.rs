//! Typed wrappers over the native metadata API (`docs/DESIGN.md` §5), served by
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
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::{Map, Value, json};

use proxmox_yew_comp::{http_get_auth, json_object_to_query};

use crate::edit::{Write, delete_body, put_body, put_text_body};
use crate::grammar::Operator;
use crate::model::{Access, DocId, GuestInfo};

/// A document view as JSON data (`format=json`).
///
/// `data` is an unordered object (`docs/DESIGN.md` §4: "Key order is preserved in the file
/// and is not a wire contract; the UI sorts"), which is exactly what the tree wants — it
/// derives its own order from the keys and the grammars (`crate::tree`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DocData {
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub data: Value,
}

/// A document view rendered as text (`format=yaml`) — the "Edit as text" dialog's input,
/// and the only place the page ever looks at YAML.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DocText {
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub text: String,
    /// Set when the stored file does not parse; the document is then only repairable
    /// through a root replace (`docs/DESIGN.md` §4).
    #[serde(default)]
    pub parse_error: Option<String>,
}

/// The result of a write.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct WriteResult {
    /// The document's digest *after* the write — what the next write in a chain sends.
    #[serde(default)]
    pub digest: String,
}

/// `GET /meta/version` — a content hash over the store, to be polled.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct VersionInfo {
    #[serde(default)]
    pub token: String,
}

/// One entry of `GET /meta/guests`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct GuestEntry {
    #[serde(default)]
    pub vmid: u32,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default, rename = "type")]
    pub guest_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// PVE tags, which decide which operator scopes apply to this guest
    /// (`docs/DESIGN.md` §3). Only answered to a caller with `VM.Audit`.
    #[serde(default, deserialize_with = "deserialize_tags")]
    pub tags: Vec<String>,
}

/// Tags as either a JSON array or PVE's own `a;b;c` string.
fn deserialize_tags<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Value::String(text) => text
            .split([';', ','])
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    })
}

/// The caller's effective grants **for one document** (`GET /meta/access?vmid=…`, or
/// `?dc=1` for the datacenter document).
///
/// Asking per document is what lets the page decide editability per row: the answer's
/// `scopes` are already resolved against this guest's selectors.
pub async fn access(doc: DocId) -> Result<Access, Error> {
    let query = match doc {
        DocId::Guest(vmid) => json!({ "vmid": vmid }),
        DocId::Datacenter => json!({ "dc": 1 }),
    };
    get_json("/meta/access", Some(query)).await
}

/// `GET /meta/operators` — every registration, readable by any authenticated user.
///
/// The tree needs these for the declared-but-unset rows and for the Access column; it is
/// never an access decision, so a page whose cluster has no registrations (or whose API
/// predates the endpoint) simply shows no Access entries.
pub async fn operators() -> Result<Vec<Operator>, Error> {
    get_json("/meta/operators", None).await
}

/// This guest's entry in `GET /meta/guests` — its tags (for the scope selectors) and its
/// name, for the identity line.
pub async fn guest_info(vmid: u32) -> Result<Option<GuestInfo>, Error> {
    let guests: Vec<GuestEntry> = get_json("/meta/guests", None).await?;
    Ok(guests
        .into_iter()
        .find(|entry| entry.vmid == vmid)
        .map(|entry| GuestInfo {
            name: entry.name,
            node: entry.node,
            guest_type: entry.guest_type,
            tags: entry.tags,
        }))
}

/// A view of a document as JSON data. An empty `view` is the whole document.
pub async fn get_data(doc: DocId, view: &str) -> Result<DocData, Error> {
    get_json(&doc.api_path(), Some(view_query(view, "json"))).await
}

/// A view of a document as YAML text.
pub async fn get_text(doc: DocId, view: &str) -> Result<DocText, Error> {
    get_json(&doc.api_path(), Some(view_query(view, "yaml"))).await
}

fn view_query(view: &str, format: &str) -> Value {
    let mut query = Map::new();
    query.insert("format".into(), json!(format));
    if !view.is_empty() {
        query.insert("view".into(), json!(view));
    }
    Value::Object(query)
}

/// `GET /meta/version`. The native module answers immediately (no long poll), so this is
/// polled on an interval.
pub async fn version() -> Result<VersionInfo, Error> {
    get_json("/meta/version", None).await
}

/// Apply one row edit: the writes it consists of, in order, each one pinned to the digest
/// the previous one returned.
///
/// Chaining the digest is what makes a two-step edit (a value and the comment key that
/// documents it) safe without a transaction: the second write cannot be racing anything
/// the first did not already see, and an intervening change by somebody else still 409s
/// the first (`docs/DESIGN.md` §4).
pub async fn apply(doc: DocId, writes: Vec<Write>, digest: &str) -> Result<WriteResult, Error> {
    let mut current = digest.to_string();
    let mut result = WriteResult {
        digest: current.clone(),
    };
    for write in writes {
        result = match &write {
            Write::Put { view, value } => {
                send_json("PUT", &doc.api_path(), put_body(view, value, &current)).await?
            }
            Write::PutText { view, text } => {
                send_json("PUT", &doc.api_path(), put_text_body(view, text, &current)).await?
            }
            // A DELETE carries its parameters in the query string, never in a body:
            // `PVE::APIServer::AnyEvent` answers a body on DELETE with
            // "501 Unexpected content for method 'DELETE'".
            Write::Delete { view } => {
                send_empty("DELETE", &doc.api_path(), delete_body(view, &current)).await?
            }
        };
        current = result.digest.clone();
    }
    Ok(result)
}

/// An API error carrying the actual HTTP status code (not a body-embedded one).
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The server message verbatim — `docs/DESIGN.md` §4 asks for exactly that.
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// True if `err` is an HTTP 409: the document changed on the server since it was loaded.
pub fn is_conflict(err: &Error) -> bool {
    status_of(err) == Some(409)
}

/// True if `err` is an HTTP 404 or 501: an endpoint this API revision does not serve.
///
/// `GET /meta/operators` is new in revision 5; a page talking to an older node must still
/// render its document, just without grammars or Access entries.
pub fn is_unimplemented(err: &Error) -> bool {
    matches!(status_of(err), Some(404) | Some(501)) || err.to_string().contains("not implemented")
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

    builder = csrf(builder, method);

    let request = builder
        .header("content-type", "application/json")
        .body(serde_json::to_string(&body)?)
        .map_err(|e| anyhow!("failed to build request: {e}"))?;

    send(request).await
}

/// A mutating request whose parameters ride in the query string and which sends no body.
async fn send_empty<T: DeserializeOwned>(
    method: &str,
    path: &str,
    params: Value,
) -> Result<T, Error> {
    let mut url = format!("/api2/json{path}");
    let query = json_object_to_query(params)?;
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query);
    }

    let mut builder = match method {
        "DELETE" => gloo_net::http::Request::delete(&url),
        _ => return Err(anyhow!("unsupported method {method}")),
    };
    builder = csrf(builder, method);

    let request = builder
        .build()
        .map_err(|e| anyhow!("failed to build request: {e}"))?;

    send(request).await
}

/// Attach the CSRF token every mutating call needs, read fresh so it survives a ticket
/// refresh.
fn csrf(builder: gloo_net::http::RequestBuilder, method: &str) -> gloo_net::http::RequestBuilder {
    match http_get_auth() {
        Some(auth) => builder.header("CSRFPreventionToken", &auth.csrfprevention_token),
        None => {
            log::warn!("pve-meta-ui: sending a {method} request with no known CSRF token");
            builder
        }
    }
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
