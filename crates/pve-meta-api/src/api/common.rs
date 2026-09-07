//! Shared logic between the `/meta/guests/{vmid}/...` and `/meta/datacenter/...` endpoints,
//! which are identical except for which [`DocId`] they operate on.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Error;
use proxmox_router::http_err;
use serde_json::{Map, Value};

use pve_meta_core::digest;
use pve_meta_core::edit;
use pve_meta_core::format::{self, Format};
use pve_meta_core::model;
use pve_meta_core::patch::{self, Op, Touched};
use pve_meta_core::path::Path as DocPath;
use pve_meta_core::store::DocId;

use crate::error::to_http;
use crate::store::store;

fn doc_id_str(id: DocId) -> String {
    match id {
        DocId::Guest(vmid) => vmid.to_string(),
        DocId::Datacenter => "datacenter".to_string(),
    }
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn touched_json(touched: &[Touched]) -> Value {
    Value::Array(
        touched
            .iter()
            .map(|t| {
                serde_json::json!({
                    "path": t.path.to_string(),
                    "op": match t.op {
                        Op::Set => "set",
                        Op::Delete => "delete",
                    },
                })
            })
            .collect(),
    )
}

/// Renders the wire representation of a document (see `docs/API.md`): `id`, `format`,
/// `digest`, `mtime`, `data` (comment keys stripped unless `comments`), plus `raw` when
/// requested.
fn document_json(
    id: DocId,
    format: Format,
    digest: &str,
    mtime: SystemTime,
    value: &Value,
    raw_text: Option<&str>,
    comments: bool,
) -> Value {
    let mut data = value.clone();
    if !comments {
        model::strip_comments(&mut data);
    }
    let mut obj = Map::new();
    obj.insert("id".into(), Value::String(doc_id_str(id)));
    obj.insert("format".into(), Value::String(format.to_string()));
    obj.insert("digest".into(), Value::String(digest.to_string()));
    obj.insert("mtime".into(), Value::from(unix_secs(mtime)));
    obj.insert("data".into(), data);
    if let Some(raw) = raw_text {
        obj.insert("raw".into(), Value::String(raw.to_string()));
    }
    Value::Object(obj)
}

/// `GET /meta/guests/{vmid}` / `GET /meta/datacenter`.
pub fn get_document(id: DocId, comments: bool, raw: bool) -> Result<Value, Error> {
    let doc = store().read(id).map_err(to_http)?;
    Ok(document_json(
        id,
        doc.format,
        &doc.digest,
        doc.mtime,
        &doc.value,
        raw.then_some(doc.raw.as_str()),
        comments,
    ))
}

/// `GET /meta/guests/{vmid}/subtree?path=...` / `GET /meta/datacenter/subtree?path=...`.
/// Comment keys are always stripped (not configurable, matching `docs/API.md`).
pub fn get_subtree(id: DocId, path: &str) -> Result<Value, Error> {
    let doc = store().read(id).map_err(to_http)?;
    let parsed = DocPath::parse(path).map_err(to_http)?;
    let mut value = doc.value;
    model::strip_comments(&mut value);
    let sub = model::get_path(&value, &parsed)
        .ok_or_else(|| http_err!(NOT_FOUND, "no data at path '{path}'"))?;
    Ok(serde_json::json!({ "data": sub, "digest": doc.digest }))
}

fn conflict_no_document(expected: &str) -> Error {
    to_http(pve_meta_core::Error::DigestMismatch {
        expected: expected.to_string(),
        actual: String::new(),
    })
}

/// `PUT /meta/guests/{vmid}` / `PUT /meta/datacenter`: merge-patch, with `dry_run` support that
/// `pve-meta-core`'s store does not provide natively.
pub fn patch_document(
    id: DocId,
    patch_value: &Value,
    digest_expected: Option<&str>,
    dry_run: bool,
) -> Result<Value, Error> {
    if !dry_run {
        let old_value = match store().read(id) {
            Ok(doc) => doc.value,
            Err(pve_meta_core::Error::NotFound(_)) => Value::Object(Map::new()),
            Err(e) => return Err(to_http(e)),
        };
        let new_doc = store().patch(id, patch_value, digest_expected).map_err(to_http)?;
        let touched = patch::diff(&old_value, &new_doc.value);
        let mut response = document_json(
            id,
            new_doc.format,
            &new_doc.digest,
            new_doc.mtime,
            &new_doc.value,
            None,
            true,
        );
        response["touched"] = touched_json(&touched);
        Ok(response)
    } else {
        let lints = patch::lint_patch(patch_value);
        if !lints.is_empty() {
            return Err(to_http(pve_meta_core::Error::Lint(lints)));
        }
        let (fmt, old_text, old_value) = match store().locate(id).map_err(to_http)? {
            Some(located) => {
                let doc = store().read(id).map_err(to_http)?;
                if let Some(expected) = digest_expected {
                    if expected != doc.digest {
                        return Err(to_http(pve_meta_core::Error::DigestMismatch {
                            expected: expected.to_string(),
                            actual: doc.digest,
                        }));
                    }
                }
                (located.format, doc.raw, doc.value)
            }
            None => {
                if let Some(expected) = digest_expected {
                    return Err(conflict_no_document(expected));
                }
                let fmt = store().default_format().map_err(to_http)?;
                let empty = Value::Object(Map::new());
                (fmt, format::dump(fmt, &empty), empty)
            }
        };
        let result = edit::apply_patch_text(fmt, &old_text, patch_value).map_err(to_http)?;
        let touched = patch::diff(&old_value, &result.value);
        let dig = digest::digest(result.text.as_bytes());
        let mut response = document_json(id, fmt, &dig, SystemTime::now(), &result.value, None, true);
        response["touched"] = touched_json(&touched);
        Ok(response)
    }
}

/// `PUT /meta/guests/{vmid}/raw` / `PUT /meta/datacenter/raw`: full-text replace, with
/// `dry_run` support.
pub fn put_raw(
    id: DocId,
    content: &str,
    format_override: Option<Format>,
    digest_expected: Option<&str>,
    dry_run: bool,
) -> Result<Value, Error> {
    if !dry_run {
        let result = store()
            .put_raw(id, content, format_override, digest_expected)
            .map_err(to_http)?;
        let mut response = document_json(
            id,
            result.document.format,
            &result.document.digest,
            result.document.mtime,
            &result.document.value,
            None,
            true,
        );
        response["touched"] = touched_json(&result.touched);
        Ok(response)
    } else {
        let located = store().locate(id).map_err(to_http)?;
        let (old_value, target_format) = match &located {
            Some(l) => {
                let doc = store().read(id).map_err(to_http)?;
                if let Some(expected) = digest_expected {
                    if expected != doc.digest {
                        return Err(to_http(pve_meta_core::Error::DigestMismatch {
                            expected: expected.to_string(),
                            actual: doc.digest,
                        }));
                    }
                }
                (doc.value, format_override.unwrap_or(l.format))
            }
            None => {
                if let Some(expected) = digest_expected {
                    return Err(conflict_no_document(expected));
                }
                (
                    Value::Object(Map::new()),
                    format_override.unwrap_or(store().default_format().map_err(to_http)?),
                )
            }
        };
        let normalized = normalize_trailing_newline(content);
        let new_value = format::parse(target_format, &normalized).map_err(to_http)?;
        let touched = patch::diff(&old_value, &new_value);
        let dig = digest::digest(normalized.as_bytes());
        let mut response = document_json(
            id,
            target_format,
            &dig,
            SystemTime::now(),
            &new_value,
            Some(normalized.as_str()),
            true,
        );
        response["touched"] = touched_json(&touched);
        Ok(response)
    }
}

fn normalize_trailing_newline(text: &str) -> String {
    let trimmed = text.trim_end_matches('\n');
    format!("{trimmed}\n")
}

/// `POST /meta/guests/{vmid}/convert` / `POST /meta/datacenter/convert`.
pub fn convert(id: DocId, to: Format, digest_expected: Option<&str>) -> Result<Value, Error> {
    let doc = store().convert(id, to, digest_expected).map_err(to_http)?;
    Ok(document_json(id, doc.format, &doc.digest, doc.mtime, &doc.value, None, true))
}

/// `DELETE /meta/guests/{vmid}` / `DELETE /meta/datacenter`.
pub fn delete(id: DocId) -> Result<Value, Error> {
    store().delete(id).map_err(to_http)?;
    Ok(Value::Null)
}

/// Parses a format name (`yaml`/`yml`/`toml`/`json`), for endpoints that take one as a string
/// parameter.
pub fn parse_format(name: &str) -> Result<Format, Error> {
    Format::from_ext(name).ok_or_else(|| http_err!(BAD_REQUEST, "unknown format '{name}'"))
}
