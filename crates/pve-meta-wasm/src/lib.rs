//! `pve-meta-wasm`: `pve-meta-core` for the browser, so the editor asks the
//! same code the server runs instead of keeping a JavaScript copy of it.
//!
//! # The ABI
//!
//! A plain `cargo build --target wasm32-unknown-unknown`; no `wasm-bindgen`,
//! no build tool beyond cargo. Four exports:
//!
//! * `pm_alloc(len) -> ptr` / `pm_free(ptr, len)` -- a buffer the caller
//!   copies its request into and releases afterwards;
//! * `pm_call(ptr, len) -> out_len` -- runs one request and leaves the
//!   response in an output buffer this module owns;
//! * `pm_output() -> ptr` -- that buffer. It is reused by the next call, so
//!   the caller copies the response out before calling again.
//!
//! A request is one UTF-8 JSON document, `{"fn": <name>, "args": [...]}`,
//! and a response is `{"ok": <value>}` or `{"err": {"message": ..., "line"?,
//! "column"?}}`. Every function in this crate takes and returns documents
//! anyway -- a `Value`, a path, an edit set -- so JSON is the interface's
//! natural type, and the glue on the JavaScript side is a dozen lines
//! ([`ui-extjs/pve-meta-tree.js`, `PVE.meta.Core`]).
//!
//! # What crosses
//!
//! The concepts, one group each ([`call`]): the **codec** (`format::parse`,
//! `format::dump`), the **path** rules, **`Effective`** (may this caller
//! read or write a path), **`Shape`** (which prefixes reach a document, what
//! governs a path, what a schema says), **`EditSet`** (staged edits), and
//! the permission rules that reach a guest. Nothing here decides anything:
//! it deserializes the wire shape the API hands the browser -- where Perl
//! has rendered `true` as `1` -- into the core's own types, and calls.
//!
//! # Not a security boundary
//!
//! The browser predicts; the server enforces. Everything this crate answers
//! the server answers again on the real write, from the same functions.
//! What a shared implementation buys is that the prediction is *right*: a
//! row the editor offers to write is a row the server will take.

use std::cell::RefCell;

use pve_meta_core::edit::{Edit, EditSet};
use pve_meta_core::error::Error;
use pve_meta_core::format::{self, Format};
use pve_meta_core::path::{self, Path};
use pve_meta_core::registry::{self, Permission, Rule, Selector};
use pve_meta_core::scopes::{self, Effective, Mode, Scope};
use pve_meta_core::shape::{self, Declared, Finding, Shape};
use pve_meta_core::{model, view, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The ABI version; bump when the request or response shape changes.
pub const ABI: u32 = 1;

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
        "path_contains" => {
            let p = a.path()?;
            let q = a.path()?;
            json!(p.is_prefix_of(&q))
        }

        // -- access: scopes::Effective -----------------------------------
        //
        // `covers` is the coverage rule on its own (a rule on `p` covers `p`,
        // its comment key `p__`, and `p.*`), for "which rules reach this
        // row"; the three `access_*` are the caller's own answers from a
        // `GET /meta/access` result.
        "covers" => {
            let prefix = a.path()?;
            let p = a.path()?;
            json!(scopes::covers(&prefix, &p))
        }
        "access_can_read" => {
            let access = a.access()?;
            json!(access.can_read(&a.path()?))
        }
        "access_can_write" => {
            let access = a.access()?;
            json!(access.can_write(&a.path()?))
        }
        "access_has_any_write" => json!(a.access()?.has_any_write()),

        // -- shape: shape::Shape -----------------------------------------
        //
        // Each takes the `GET /meta/prefixes` listing and the guest's tags
        // (from `GET /meta/access`), and builds the Shape afresh: a listed
        // file that did not load, or whose selector this cannot read, reaches
        // nothing. A registry document passes its meta-schema as one entry
        // with the empty prefix, which is a prefix of everything.
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
                .map(|(p, schema)| {
                    let owner = shape.governing(&p).expect("indexed paths are governed");
                    json!({"path": p, "prefix": owner.prefix, "schema": schema})
                })
                .collect();
            json!(index)
        }
        "shape_findings" => {
            let shape = a.shape()?;
            let doc = a.value()?;
            serde_json::to_value(shape.findings(&doc))?
        }
        "findings_introduced" => {
            let before: Vec<Finding> = a.parsed()?;
            let after: Vec<Finding> = a.parsed()?;
            let changed: Vec<Path> = a.parsed()?;
            serde_json::to_value(shape::introduced(&before, &after, &changed))?
        }

        // -- permissions: registry::rules_reaching ------------------------
        //
        // The `GET /meta/permissions` listing and the guest's tags; every
        // rule that reaches the guest, with the file it came from. A file
        // that did not load grants nothing.
        "rules_reaching" => {
            let files = a.permissions()?;
            let tags = a.tags()?;
            let rules: Vec<Value> = registry::rules_reaching(&files, &tags)
                .map(|(file, rule)| {
                    json!({
                        "name": file.name, "authid": file.authid,
                        "prefix": rule.prefix, "mode": rule.mode, "selector": rule.selector,
                    })
                })
                .collect();
            json!(rules)
        }

        // -- edits: edit::EditSet ----------------------------------------
        "edits_stage" => {
            let mut set: EditSet = a.parsed()?;
            let edit: Edit = a.parsed()?;
            set.stage(edit);
            serde_json::to_value(set)?
        }
        "edits_under" => {
            let set: EditSet = a.parsed()?;
            let path = a.path()?;
            serde_json::to_value(set.under(&path).collect::<Vec<_>>())?
        }
        "edits_discard_under" => {
            let mut set: EditSet = a.parsed()?;
            let path = a.path()?;
            set.discard_under(&path);
            serde_json::to_value(set)?
        }
        "edits_apply" => {
            let stored = a.value()?;
            let set: EditSet = a.parsed()?;
            set.apply(&stored)?
        }
        "edits_between" => {
            let stored = a.value()?;
            let edited = a.value()?;
            serde_json::to_value(EditSet::between(&stored, &edited))?
        }
        "edits_write_view" => {
            let set: EditSet = a.parsed()?;
            json!(set.write_view())
        }
        "changed_paths" => {
            let was = a.value()?;
            let now = a.value()?;
            json!(pve_meta_core::edit::changed_paths(&was, &now))
        }
        "same_ordered" => {
            let x = a.value()?;
            let y = a.value()?;
            json!(model::same_ordered(&x, &y))
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

    fn tags(&mut self) -> Result<Vec<String>, CallError> {
        self.parsed()
    }

    /// A `GET /meta/access` result.
    fn access(&mut self) -> Result<Effective, CallError> {
        let v = self.next()?;
        let scopes: Vec<Scope> = match v.get("scopes") {
            None | Some(Value::Null) => Vec::new(),
            Some(s) => serde_json::from_value(s.clone())
                .map_err(|e| bad(format!("{}: scopes: {e}", self.name)))?,
        };
        Ok(Effective {
            full_read: v.get("read").is_some_and(truthy),
            full_write: v.get("write").is_some_and(truthy),
            scopes,
        })
    }

    /// A `GET /meta/prefixes` listing plus the guest's tags, as a Shape.
    fn shape(&mut self) -> Result<Shape, CallError> {
        let entries: Vec<WirePrefix> = self.parsed()?;
        let tags = self.tags()?;
        Ok(Shape::new(entries.into_iter().filter_map(WirePrefix::declared), &tags))
    }

    /// A `GET /meta/permissions` listing.
    fn permissions(&mut self) -> Result<Vec<Permission>, CallError> {
        let entries: Vec<WirePermission> = self.parsed()?;
        Ok(entries.into_iter().filter_map(WirePermission::permission).collect())
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

/// One row of `GET /meta/prefixes`: a prefix, or the name of a file that
/// did not load (`error`), which describes nothing.
#[derive(Deserialize)]
struct WirePrefix {
    prefix: String,
    #[serde(default)]
    selector: Option<Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    schema: Option<Value>,
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
            selector: Selector::from_wire(self.selector.as_ref()?).ok()?,
            description: self.description,
            schema: self.schema,
        })
    }
}

/// One row of `GET /meta/permissions`, likewise.
#[derive(Deserialize)]
struct WirePermission {
    name: String,
    #[serde(default)]
    authid: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    rules: Vec<WireRule>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct WireRule {
    prefix: String,
    mode: Mode,
    selector: Value,
}

impl WirePermission {
    fn permission(self) -> Option<Permission> {
        if self.error.is_some() {
            return None;
        }
        let mut rules = Vec::with_capacity(self.rules.len());
        for r in self.rules {
            rules.push(Rule {
                prefix: Path::parse(&r.prefix).ok()?,
                mode: r.mode,
                selector: Selector::from_wire(&r.selector).ok()?,
            });
        }
        Some(Permission {
            name: self.name,
            authid: self.authid?,
            description: self.description,
            rules,
            origin: registry::Origin::Cluster,
            overrides: false,
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
        assert!(err("covers", json!(["a", "a b"])).message.contains("invalid path"));
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
    }

    #[test]
    fn access_reads_perls_booleans() {
        let auditor = json!({"read": 1, "write": 0, "scopes": [], "tags": ["t"]});
        assert_eq!(ok("access_can_read", json!([auditor, "anything"])), json!(true));
        assert_eq!(ok("access_can_write", json!([auditor, "anything"])), json!(false));
        assert_eq!(ok("access_has_any_write", json!([auditor])), json!(false));

        let scoped = json!({"read": 0, "write": 0, "scopes": [{"prefix": "traefik", "mode": "rw"}, {"prefix": "netbird", "mode": "ro"}]});
        assert_eq!(ok("access_can_write", json!([scoped, "traefik.spec.host"])), json!(true));
        assert_eq!(ok("access_can_write", json!([scoped, "traefik__"])), json!(true));
        assert_eq!(ok("access_can_write", json!([scoped, "netbird.groups"])), json!(false));
        assert_eq!(ok("access_can_read", json!([scoped, "netbird.groups"])), json!(true));
        assert_eq!(ok("access_can_read", json!([scoped, ""])), json!(false), "a scope never covers the root");
        assert_eq!(ok("access_has_any_write", json!([scoped])), json!(true));
        assert_eq!(ok("access_has_any_write", json!([{}])), json!(false));
        assert_eq!(ok("access_has_any_write", json!([{"write": true}])), json!(true));
    }

    #[test]
    fn a_shape_is_built_from_the_listing_and_ignores_what_did_not_load() {
        let listing = json!([
            {"prefix": "homelab", "selector": {"all": 1}, "schema": {"type": "object", "properties": {"notes": {"type": "string"}}}},
            {"prefix": "homelab.docker", "selector": {"all": true}},
            {"prefix": "traefik", "selector": {"tag": "traefik"}, "schema": {"type": "object"}},
            {"prefix": "broken", "error": "selector: nonsense"},
            {"prefix": "odd", "selector": {"pool": "p"}},
        ]);
        assert_eq!(ok("shape_prefixes", json!([listing, []])), json!(["homelab.docker", "homelab"]));
        assert_eq!(ok("shape_prefixes", json!([listing, ["traefik"]])), json!(["homelab.docker", "homelab", "traefik"]));
        assert_eq!(ok("shape_governing", json!([listing, [], "homelab.docker.compose"])), json!("homelab.docker"));
        assert_eq!(ok("shape_governing", json!([listing, [], "traefik.spec"])), Value::Null);
        assert_eq!(ok("shape_governing", json!([listing, [], "broken.x"])), Value::Null);
        let index = ok("shape_schema_index", json!([listing, []]));
        assert_eq!(index[0]["path"], "homelab");
        assert_eq!(index[1], json!({"path": "homelab.notes", "prefix": "homelab", "schema": {"type": "string"}}));
        let findings = ok("shape_findings", json!([listing, [], {"homelab": {"notes": 5}}]));
        assert_eq!(findings["findings"][0]["path"], "homelab.notes");

        // A registry document: its meta-schema rooted at the document.
        let rooted = json!([{"prefix": "", "selector": {"all": true}, "schema": {"type": "object", "properties": {"authid": {"type": "string"}}}}]);
        assert_eq!(ok("shape_governing", json!([rooted, [], "rules"])), json!(""));
        assert_eq!(ok("shape_findings", json!([rooted, [], {"authid": 1}]))["findings"][0]["msg"], "expected string");
    }

    #[test]
    fn rules_reaching_the_guest_carry_their_file() {
        let listing = json!([
            {"name": "traefik", "authid": "svc@pve!traefik", "rules": [
                {"prefix": "traefik", "mode": "rw", "selector": {"tag": "traefik"}},
                {"prefix": "netbird", "mode": "ro", "selector": {"all": 1}},
            ]},
            {"name": "broken", "error": "authid: nope"},
        ]);
        let got = ok("rules_reaching", json!([listing, []]));
        assert_eq!(got.as_array().unwrap().len(), 1);
        assert_eq!(got[0]["prefix"], "netbird");
        assert_eq!(got[0]["name"], "traefik");
        assert_eq!(got[0]["selector"], json!({"all": true}));
        let got = ok("rules_reaching", json!([listing, ["traefik"]]));
        assert_eq!(got.as_array().unwrap().len(), 2);
    }

    #[test]
    fn edits_go_through_the_edit_set() {
        let stored = json!({"zebra": 1, "alpha": 2});
        let set = ok("edits_stage", json!([[], {"path": "alpha", "op": "set", "value": 9}]));
        let planned = ok("edits_apply", json!([stored, set]));
        assert_eq!(serde_json::to_string(&planned).unwrap(), r#"{"zebra":1,"alpha":9}"#);
        assert_eq!(ok("edits_write_view", json!([set])), json!("alpha"));
        assert_eq!(ok("edits_write_view", json!([[]])), Value::Null);
        assert_eq!(
            ok("edits_between", json!([stored, {"zebra": 1}])),
            json!([{"path": "alpha", "op": "delete"}])
        );
        assert_eq!(ok("changed_paths", json!([stored, {"alpha": 2, "zebra": 1}])), json!([]));
        assert_eq!(ok("same_ordered", json!([stored, {"alpha": 2, "zebra": 1}])), json!(false));
        assert_eq!(ok("edits_under", json!([set, "alpha"])).as_array().unwrap().len(), 1);
        assert_eq!(ok("edits_discard_under", json!([set, ""])), json!([]));
        let e = err("edits_apply", json!([{"a": [1]}, [{"path": "a.b", "op": "set", "value": 1}]]));
        assert!(e.message.contains("never through an array"), "{e:?}");
    }
}
