//! `pve-meta-wasm`: `pve-meta-core` for the browser, so the editor asks the
//! same code the server runs instead of keeping a JavaScript copy of it.
//!
//! # The ABI
//!
//! A plain `cargo build --target wasm32-unknown-unknown`; no `wasm-bindgen`,
//! no build tool beyond cargo. Five exports:
//!
//! * `pm_alloc(len) -> ptr` / `pm_free(ptr, len)` -- a buffer the caller
//!   copies its request into and releases afterwards;
//! * `pm_call(ptr, len) -> out_len` -- runs one request and leaves the
//!   response in an output buffer this module owns;
//! * `pm_output() -> ptr` -- that buffer. It is reused by the next call, so
//!   the caller copies the response out before calling again;
//! * `pm_abi() -> u32` -- [`ABI`], which the glue checks on attach so a
//!   `.wasm` and a script from different builds fail loudly rather than
//!   mis-read each other.
//!
//! A request is one UTF-8 JSON document, `{"fn": <name>, "args": [...]}`,
//! and a response is `{"ok": <value>}` or `{"err": {"message": ..., "line"?,
//! "column"?}}`. Every function in this crate takes and returns documents
//! anyway -- a `Value`, a path -- so JSON is the interface's natural type,
//! and the glue on the JavaScript side is a dozen lines
//! ([`ui-extjs/pve-meta-tree.js`, `PVE.meta.Core`]).
//!
//! # What crosses
//!
//! The concepts, one group each ([`call`]): the **codec** (`format::parse`,
//! `format::dump`), the **path** rules and **`Shape`** (which prefixes reach
//! a document, what governs a path, what a schema says). Nothing here
//! decides anything: it deserializes the wire shape the API hands the
//! browser -- where Perl has rendered `true` as `1` -- into the core's own
//! types, and calls.
//!
//! # Not a security boundary
//!
//! The browser predicts; the server enforces. Everything this crate answers
//! the server answers again on the real write, from the same functions.
//! What a shared implementation buys is that the prediction is *right*: a
//! row the editor offers to write is a row the server will take.

use std::cell::RefCell;

use pve_meta_core::error::Error;
use pve_meta_core::format::{self, Format};
use pve_meta_core::path::{self, Path};
use pve_meta_core::registry;
use pve_meta_core::shape::{Declared, Shape};
use pve_meta_core::{view, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The ABI version; bump when the request or response shape changes.
pub const ABI: u32 = 3;

thread_local! {
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// # Safety
/// Returns memory the caller owns until it passes it back to [`pm_free`]
/// with the same `len`. A zero `len` yields a dangling, non-null pointer
/// that must still be freed with `len == 0`.
#[no_mangle]
pub extern "C" fn pm_alloc(len: u32) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr` must have come from [`pm_alloc`] with this `len`.
#[no_mangle]
pub unsafe extern "C" fn pm_free(ptr: *mut u8, len: u32) {
    drop(Vec::from_raw_parts(ptr, 0, len as usize));
}

/// # Safety
/// `ptr..ptr+len` must be readable. Not UTF-8 or not JSON is an ordinary
/// `err` response, never a trap.
#[no_mangle]
pub unsafe extern "C" fn pm_call(ptr: *const u8, len: u32) -> u32 {
    let input = std::slice::from_raw_parts(ptr, len as usize);
    let response = dispatch(input);
    OUTPUT.with(|out| {
        let mut out = out.borrow_mut();
        out.clear();
        serde_json::to_writer(&mut *out, &response).expect("a response serializes");
        out.len() as u32
    })
}

#[no_mangle]
pub extern "C" fn pm_output() -> *const u8 {
    OUTPUT.with(|out| out.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn pm_abi() -> u32 {
    ABI
}

// ---------------------------------------------------------------------
// Request and response
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct Request {
    #[serde(rename = "fn")]
    name: String,
    #[serde(default)]
    args: Vec<Value>,
}

/// What the caller gets back. A parse error carries where the parser
/// stopped, for a marker on that line.
#[derive(Debug, Serialize, PartialEq)]
pub struct CallError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

impl From<Error> for CallError {
    fn from(e: Error) -> Self {
        let at = match &e {
            Error::Parse { at, .. } => *at,
            _ => None,
        };
        CallError {
            message: e.to_string(),
            line: at.map(|l| l.line),
            column: at.map(|l| l.column),
        }
    }
}

impl From<serde_json::Error> for CallError {
    fn from(e: serde_json::Error) -> Self {
        CallError { message: e.to_string(), line: None, column: None }
    }
}

fn bad(msg: impl Into<String>) -> CallError {
    CallError { message: msg.into(), line: None, column: None }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response {
    Ok { ok: Value },
    Err { err: CallError },
}

fn dispatch(input: &[u8]) -> Response {
    let run = || -> Result<Value, CallError> {
        let text = std::str::from_utf8(input).map_err(|e| bad(format!("request is not UTF-8: {e}")))?;
        let req: Request = serde_json::from_str(text).map_err(|e| bad(format!("request is not JSON: {e}")))?;
        call(&req.name, &req.args)
    };
    match run() {
        Ok(ok) => Response::Ok { ok },
        Err(err) => Response::Err { err },
    }
}

// ---------------------------------------------------------------------
// The functions
// ---------------------------------------------------------------------

/// One request, by name. Grouped by the concept each name is a method of.
pub fn call(name: &str, args: &[Value]) -> Result<Value, CallError> {
    let mut a = Args::new(name, args);
    let out = match name {
        "abi" => json!(ABI),

        // -- codec: format::parse / format::dump ---------------------
        //
        // `parse` reads a *buffer*, which may be a whole document or a view
        // of one, so it is `view::parse`: no lint and no "must be a map" --
        // the one lint runs on the planned document, server side, where its
        // findings name real paths. A null top level -- an empty buffer, or
        // `~` -- is the empty map, because the model has no nulls and an
        // empty editor is an empty document.
        "parse" => {
            let fmt = a.format()?;
            let text = a.str()?;
            match view::parse(text, fmt)? {
                Value::Null => json!({}),
                v => v,
            }
        }
        "dump" => {
            let fmt = a.format()?;
            let value = a.value()?;
            json!(format::dump(fmt, &value))
        }

        // -- paths -----------------------------------------------------
        //
        // Why a key name is not one: `null` if it is fine. A dotted path is
        // checked segment by segment, since a Key field takes one. The
        // caller words it; this only says which rule, and which character.
        "key_path_check" => {
            let text = a.str()?;
            if text.is_empty() {
                json!({"reason": "empty"})
            } else if let Some(bad_seg) = text.split('.').find(|s| !path::is_valid_segment(s)) {
                if bad_seg.is_empty() {
                    json!({"reason": "empty_segment"})
                } else {
                    let c = path::invalid_char(bad_seg).expect("a non-empty invalid segment has one");
                    json!({"reason": "char", "segment": bad_seg, "char": c.to_string()})
                }
            } else {
                Value::Null
            }
        }
        "file_name_valid" => json!(registry::is_valid_file_name(a.str()?)),

        // -- shape: shape::Shape -----------------------------------------
        //
        // Each takes the rows `GET /meta/prefixes?id=` returns for this
        // guest -- already resolved (selector-matched, node override
        // applied) -- and builds the Shape afresh: a listed file that did
        // not load reaches nothing. A registry document passes its
        // meta-schema as one entry with the empty prefix, which is a prefix
        // of everything.
        "shape_prefixes" => {
            let shape = a.shape()?;
            json!(shape.prefixes().iter().map(|d| d.prefix.to_string()).collect::<Vec<_>>())
        }
        "shape_governing" => {
            let shape = a.shape()?;
            let path = a.path()?;
            json!(shape.governing(&path).map(|d| d.prefix.to_string()))
        }
        "shape_schema_index" => {
            let shape = a.shape()?;
            let index: Vec<Value> = shape
                .schema_index()
                .into_iter()
                .map(|d| {
                    let owner = shape.governing(&d.path).expect("indexed paths are governed");
                    json!({
                        "path": d.path,
                        "prefix": owner.prefix,
                        "schema": d.schema,
                        "hidden": d.hidden,
                    })
                })
                .collect();
            json!(index)
        }
        "shape_findings" => {
            let shape = a.shape()?;
            let doc = a.value()?;
            serde_json::to_value(shape.findings(&doc))?
        }

        // Equality of two values, maps as sets: order is not a value (`docs/DESIGN.md` §2).
        "same" => {
            let x = a.value()?;
            let y = a.value()?;
            json!(x == y)
        }

        other => return Err(bad(format!("unknown function '{other}'"))),
    };
    a.done()?;
    Ok(out)
}

/// The positional arguments of one request, consumed left to right and
/// converted into the core's types -- the wire shapes the API hands the
/// browser (Perl booleans as `1`/`0`, listed files that carry an `error`)
/// are read here and nowhere else.
struct Args<'a> {
    name: &'a str,
    args: std::slice::Iter<'a, Value>,
    taken: usize,
}

impl<'a> Args<'a> {
    fn new(name: &'a str, args: &'a [Value]) -> Self {
        Args { name, args: args.iter(), taken: 0 }
    }

    fn next(&mut self) -> Result<&'a Value, CallError> {
        self.taken += 1;
        self.args
            .next()
            .ok_or_else(|| bad(format!("{}: missing argument {}", self.name, self.taken)))
    }

    fn done(mut self) -> Result<(), CallError> {
        match self.args.next() {
            Some(_) => Err(bad(format!("{}: too many arguments", self.name))),
            None => Ok(()),
        }
    }

    fn str(&mut self) -> Result<&'a str, CallError> {
        let n = self.taken + 1;
        self.next()?
            .as_str()
            .ok_or_else(|| bad(format!("{}: argument {n} must be a string", self.name)))
    }

    fn value(&mut self) -> Result<Value, CallError> {
        Ok(self.next()?.clone())
    }

    fn parsed<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T, CallError> {
        let n = self.taken + 1;
        serde_json::from_value(self.next()?.clone())
            .map_err(|e| bad(format!("{}: argument {n}: {e}", self.name)))
    }

    fn format(&mut self) -> Result<Format, CallError> {
        Ok(self.str()?.parse::<Format>()?)
    }

    fn path(&mut self) -> Result<Path, CallError> {
        Ok(Path::parse(self.str()?)?)
    }

    /// The rows `GET /meta/prefixes?id=` returns for this guest, as a
    /// `Shape`: already resolved (selector-matched, node override applied),
    /// so nothing here matches a tag.
    fn shape(&mut self) -> Result<Shape, CallError> {
        let entries: Vec<WirePrefix> = self.parsed()?;
        Ok(Shape::new(entries.into_iter().filter_map(WirePrefix::declared)))
    }
}

/// Perl's truth: `1`, `true`, a non-empty string other than `"0"`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !(s.is_empty() || s == "0"),
        Value::Null => false,
        _ => true,
    }
}

/// One row of `GET /meta/prefixes?id=`: a prefix already resolved for this
/// guest, or the name of a file that did not load (`error`), which describes
/// nothing.
#[derive(Deserialize)]
struct WirePrefix {
    prefix: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    schema: Option<Value>,
    #[serde(default)]
    enforce: Option<Value>,
    #[serde(default)]
    hidden: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

impl WirePrefix {
    fn declared(self) -> Option<Declared> {
        if self.error.is_some() {
            return None;
        }
        Some(Declared {
            prefix: Path::parse(&self.prefix).ok()?,
            description: self.description,
            schema: self.schema,
            // Perl's `1`/`0` on the wire.
            enforce: self.enforce.as_ref().is_some_and(truthy),
            hidden: self.hidden.as_ref().is_some_and(truthy),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn ok(name: &str, args: Value) -> Value {
        call(name, args.as_array().unwrap()).unwrap_or_else(|e| panic!("{name}: {e:?}"))
    }

    fn err(name: &str, args: Value) -> CallError {
        call(name, args.as_array().unwrap()).expect_err("should fail")
    }

    #[test]
    fn the_round_trip_is_the_raw_abi_not_just_call() {
        // Through the exported functions, as the JavaScript glue uses them.
        let req = br#"{"fn":"parse","args":["yaml","a: 1\nb: [x, y]\n"]}"#;
        let ptr = pm_alloc(req.len() as u32);
        unsafe { std::ptr::copy_nonoverlapping(req.as_ptr(), ptr, req.len()) };
        let len = unsafe { pm_call(ptr, req.len() as u32) };
        unsafe { pm_free(ptr, req.len() as u32) };
        let out = unsafe { std::slice::from_raw_parts(pm_output(), len as usize) };
        assert_eq!(std::str::from_utf8(out).unwrap(), r#"{"ok":{"a":1,"b":["x","y"]}}"#);

        // Garbage in is an error out, never a trap.
        let bad = b"\xff\xfe";
        let ptr = pm_alloc(2);
        unsafe { std::ptr::copy_nonoverlapping(bad.as_ptr(), ptr, 2) };
        let len = unsafe { pm_call(ptr, 2) };
        unsafe { pm_free(ptr, 2) };
        let out = unsafe { std::slice::from_raw_parts(pm_output(), len as usize) };
        assert!(std::str::from_utf8(out).unwrap().contains("not UTF-8"));
        assert_eq!(pm_abi(), ABI);
    }

    #[test]
    fn a_parse_error_says_where() {
        let e = err("parse", json!(["yaml", "a: 1\nb: [\n"]));
        assert_eq!(e.line, Some(3), "{e:?}");
        let e = err("parse", json!(["yaml", "a: &x 1\nb: *x\n"]));
        assert!(e.message.contains("anchor"));
        assert_eq!(e.line, Some(1));
        let e = err("parse", json!(["json", "{\"a\": 1,\n}"]));
        assert_eq!(e.line, Some(2));
        assert_eq!(ok("parse", json!(["yaml", ""])), json!({}));
        assert_eq!(ok("dump", json!(["yaml", {"b": 1, "a": [1]}])), json!("b: 1\na:\n- 1\n"));
    }

    #[test]
    fn arguments_are_checked() {
        assert!(err("parse", json!(["yaml"])).message.contains("missing argument 2"));
        assert!(err("parse", json!(["yaml", "a: 1", "extra"])).message.contains("too many"));
        assert!(err("parse", json!([1, "a: 1"])).message.contains("must be a string"));
        assert!(err("parse", json!(["toml", "a = 1"])).message.contains("unknown format"));
        assert!(err("nope", json!([])).message.contains("unknown function"));
        assert!(err("shape_governing", json!([[], "a b"])).message.contains("invalid path"));
    }

    #[test]
    fn key_path_check_names_the_rule_and_the_character() {
        assert_eq!(ok("key_path_check", json!(["homelab.docker.port"])), Value::Null);
        assert_eq!(ok("key_path_check", json!(["documented__"])), Value::Null);
        assert_eq!(ok("key_path_check", json!([""])), json!({"reason": "empty"}));
        assert_eq!(ok("key_path_check", json!(["a..b"])), json!({"reason": "empty_segment"}));
        assert_eq!(
            ok("key_path_check", json!(["ok.bad key"])),
            json!({"reason": "char", "segment": "bad key", "char": " "})
        );
        assert_eq!(ok("key_path_check", json!(["a/b"]))["char"], "/");
        assert_eq!(ok("file_name_valid", json!(["homelab.docker"])), json!(true));
        assert_eq!(ok("file_name_valid", json!(["my file"])), json!(false));
        // The length bound is part of the rule, so the New dialog refuses what
        // the API's `maxLength => 128` refuses.
        assert_eq!(ok("file_name_valid", json!(["a".repeat(128)])), json!(true));
        assert_eq!(ok("file_name_valid", json!(["a".repeat(129)])), json!(false));
    }

    #[test]
    fn a_shape_is_built_from_the_resolved_listing_and_ignores_what_did_not_load() {
        // The rows `GET /meta/prefixes?id=` returns: already resolved for the
        // guest (selector-matched, node override applied), so no `selector`
        // and no tags argument here -- unlike `a_wire_selector...` in
        // `pve_meta_core::registry`, which is the one place selector-matching
        // is still tested.
        let listing = json!([
            {"prefix": "homelab", "schema": {"type": "object", "properties": {"notes": {"type": "string"}}}},
            {"prefix": "homelab.docker"},
            {"prefix": "broken", "error": "selector: nonsense"},
        ]);
        assert_eq!(ok("shape_prefixes", json!([listing])), json!(["homelab.docker", "homelab"]));
        assert_eq!(ok("shape_governing", json!([listing, "homelab.docker.compose"])), json!("homelab.docker"));
        assert_eq!(ok("shape_governing", json!([listing, "broken.x"])), Value::Null);
        let index = ok("shape_schema_index", json!([listing]));
        assert_eq!(index[0]["path"], "homelab");
        // `hidden` rides with every indexed path: the editor decides from it whether
        // to offer a row before anything is stored there (decision 018).
        assert_eq!(
            index[1],
            json!({
                "path": "homelab.notes",
                "prefix": "homelab",
                "schema": {"type": "string"},
                "hidden": false,
            })
        );
        let findings = ok("shape_findings", json!([listing, {"homelab": {"notes": 5}}]));
        assert_eq!(findings, json!([{"path": "homelab.notes", "msg": "expected string"}]));
        // An enforcing prefix's findings say so, in Perl's spelling of true.
        let strict = json!([{"prefix": "homelab", "enforce": 1, "schema": {"type": "object", "properties": {"notes": {"type": "string"}}}}]);
        assert_eq!(
            ok("shape_findings", json!([strict, {"homelab": {"notes": 5}}])),
            json!([{"path": "homelab.notes", "msg": "expected string", "enforced": true}])
        );

        // A registry document: its meta-schema rooted at the document.
        let rooted = json!([{"prefix": "", "schema": {"type": "object", "properties": {"authid": {"type": "string"}}}}]);
        assert_eq!(ok("shape_governing", json!([rooted, "rules"])), json!(""));
        assert_eq!(ok("shape_findings", json!([rooted, {"authid": 1}]))[0]["msg"], "expected string");
        // A format check rides in the same list, at its place in the one order.
        let fmt = json!([{"prefix": "t", "schema": {"type": "object", "properties": {"host": {"type": "string", "format": "dns-name"}, "z": {"type": "integer"}}}}]);
        assert_eq!(
            ok("shape_findings", json!([fmt, {"t": {"host": "h", "z": "no"}}])),
            json!([{"path": "t.host", "format": "dns-name", "value": "h"}, {"path": "t.z", "msg": "expected integer"}])
        );
    }
}
