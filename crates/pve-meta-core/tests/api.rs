//! Integration tests for `pve_meta_core::api`.

use pretty_assertions::assert_eq;
use pve_meta_core::api::{
    self, access, delete_document, get_document, list_guests, parse_id, prefixes_list,
    put_document, version, ApiError, ApiPutResult, ApiViewDocument, CallerAcl, GuestInput,
};
use pve_meta_core::registry::{self, NodeName, Origin, PrefixDef, RegistryFailure};
use pve_meta_core::store::{DocId, MetaStore, RegistryKind};
use serde_json::json;

/// A store over a fresh tempdir, with its registry directory inside it
/// rather than the machine's real one (see `tests/store.rs` for why). No
/// global state: every test owns its own root, so the suite runs in
/// parallel like the rest of the crate's.
fn store() -> (tempfile::TempDir, MetaStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MetaStore::with_registry_dirs(dir.path(), vec![dir.path().join("registry/prefixes")]);
    (dir, store)
}

fn full() -> CallerAcl {
    CallerAcl {
        authid: "root@pam".to_string(),
        read: true,
        write: true,
        tags: vec![],
        node: None,
    }
}

/// `VM.Audit` and no `VM.Config.Options`.
fn read_only() -> CallerAcl {
    CallerAcl {
        authid: "auditor@pve".to_string(),
        read: true,
        write: false,
        tags: vec![],
        node: None,
    }
}

/// `VM.Config.Options` and no `VM.Audit` -- a PVE ACL can grant one without
/// the other (`docs/DESIGN.md` §4).
fn write_only() -> CallerAcl {
    CallerAcl {
        authid: "writer@pve".to_string(),
        read: false,
        write: true,
        tags: vec![],
        node: None,
    }
}

fn none() -> CallerAcl {
    CallerAcl {
        authid: "nobody@pve".to_string(),
        ..Default::default()
    }
}

fn seed(store: &MetaStore, id: &str, text: &str) {
    store.put_raw(&parse_id(id).unwrap(), text, None).unwrap();
}

fn read_raw(store: &MetaStore, id: &str) -> Option<String> {
    store.read(&parse_id(id).unwrap()).ok().map(|d| d.raw)
}

fn status(err: &ApiError) -> u16 {
    err.status
}

fn get(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    fmt: &str,
    acl: &CallerAcl,
) -> Result<ApiViewDocument, ApiError> {
    // `comments`: every rule below but the notes' own is about the stored content.
    get_document(store, id, view, fmt, true, acl)
}

#[allow(clippy::too_many_arguments)]
fn put(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    fmt: &str,
    payload: &str,
    mode: &str,
    digest: Option<&str>,
    dry_run: bool,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    put_with(store, &[], id, view, fmt, payload, mode, digest, dry_run, false, acl)
}

#[allow(clippy::too_many_arguments)]
fn put_with(
    store: &MetaStore,
    prefixes: &[PrefixDef],
    id: &str,
    view: Option<&str>,
    fmt: &str,
    payload: &str,
    mode: &str,
    digest: Option<&str>,
    dry_run: bool,
    force: bool,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    put_document(store, prefixes, id, view, fmt, payload, mode, digest, dry_run, force, true, acl)
}

/// The enforcing prefix the schema-gate tests use: `traefik`, reaching every
/// guest, with a typed `port` and a `format`-checked `host`.
fn enforcing(enforce: bool) -> Vec<PrefixDef> {
    let text = format!(
        "selector: {{all: true}}\nenforce: {enforce}\nschema:\n  type: object\n  properties:\n    port: {{type: integer}}\n    host: {{type: string, format: dns-name}}\n"
    );
    vec![pve_meta_core::registry::parse_prefix("traefik", &text).unwrap()]
}

#[test]
fn an_enforcing_prefix_refuses_what_the_write_gets_wrong_and_only_that() {
    let (_dir, store) = store();
    let strict = enforcing(true);
    let wrong = r#"{"port": "eighty"}"#;

    // Refused, naming the path.
    let err = put_with(&store, &strict, "100", Some("traefik"), "json", wrong, "replace", None, false, false, &full())
        .unwrap_err();
    assert_eq!(status(&err), 422, "{err}");
    assert!(err.msg.contains("traefik.port") && err.msg.contains("force=1"), "{err}");
    assert!(read_raw(&store, "100").is_none(), "nothing was written");
    // A dry run answers the same.
    let err = put_with(&store, &strict, "100", Some("traefik"), "json", wrong, "replace", None, true, false, &full())
        .unwrap_err();
    assert_eq!(status(&err), 422);

    // `force` stores it anyway -- the deliberate act.
    put_with(&store, &strict, "100", Some("traefik"), "json", wrong, "replace", None, false, true, &full()).unwrap();
    assert_eq!(read_raw(&store, "100").as_deref(), Some("traefik:\n  port: eighty\n"));

    // The document is now wrong under `traefik`; a write elsewhere is not
    // answerable for that and goes through.
    put_with(&store, &strict, "100", Some("other"), "json", r#"{"k": 1}"#, "replace", None, false, false, &full()).unwrap();
    // ... but writing a differently-wrong value onto the wrong key still is.
    let err = put_with(&store, &strict, "100", Some("traefik.port"), "json", r#""ninety""#, "replace", None, false, false, &full())
        .unwrap_err();
    assert_eq!(status(&err), 422);
    // Fixing it is fine, of course.
    put_with(&store, &strict, "100", Some("traefik.port"), "json", "80", "replace", None, false, false, &full()).unwrap();

    // A format check is never enforced: the server cannot judge a dns-name.
    put_with(&store, &strict, "100", Some("traefik.host"), "json", r#""not a host!!""#, "replace", None, false, false, &full()).unwrap();

    // Without `enforce`, the same schema is advisory and the same write goes through.
    put_with(&store, &enforcing(false), "100", Some("traefik.port"), "json", r#""eighty""#, "replace", None, false, false, &full()).unwrap();

    // `put_document` trusts its `prefixes` argument as already resolved for
    // this guest (`api::effective_prefixes`/`Registry::prefixes_for_guest`,
    // which selector-matching now lives behind): an empty set -- what a guest
    // outside every selector resolves to -- enforces nothing, whatever the
    // prefix directory declares elsewhere.
    put_with(&store, &[], "100", Some("traefik.port"), "json", r#""x""#, "replace", None, false, false, &full()).unwrap();
}

fn del(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    delete_document(store, id, view, digest, acl)
}

// -- version polling ----------------------------------------------------

#[test]
fn version_is_one_unscoped_token_that_moves_on_a_write() {
    // `api::version` is a thin wrapper over `MetaStore::version` -- the token
    // itself is `tests/store.rs`'s to pin; this only pins the wire shape.
    let (_dir, store) = store();
    let before = version(&store).unwrap().token;
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    assert_ne!(version(&store).unwrap().token, before);
}

// -- write authorization is `acl.write` alone (`docs/DESIGN.md` §4) -----

#[test]
fn a_caller_without_write_access_cannot_create_structure_through_a_write() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  spec:\n    host: ct100.example\n");
    let before = read_raw(&store, "100").unwrap();

    for (view, mode, payload) in [
        (Some("zzz_hacked.deep"), "merge", "{}"),
        (Some("zzz_hacked"), "merge", "{}"),
        (Some("zzz_hacked.deep"), "replace", "{}"),
        (None, "merge", "{}"),
    ] {
        let err = put(&store, "100", view, "json", payload, mode, None, false, &none())
            .expect_err("must be refused");
        assert_eq!(status(&err), 403, "{view:?}/{mode}: {err}");
        assert_eq!(read_raw(&store, "100").unwrap(), before, "{view:?}/{mode} mutated");
    }
    let err = del(&store, "100", None, None, &none()).expect_err("root delete");
    assert_eq!(status(&err), 403, "{err}");
}

#[test]
fn write_access_does_not_require_read_access() {
    // `docs/DESIGN.md` §4: there is no "must read what you write" rule. A
    // PVE ACL can grant `VM.Config.Options` without `VM.Audit`, and this is
    // the one place that survives all the way to the root view.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    put(&store, "100", None, "json", r#"{"a": 1}"#, "replace", None, false, &write_only())
        .expect("write access alone is enough to replace the whole document");
    assert_eq!(read_raw(&store, "100").as_deref(), Some("a: 1\n"));

    del(&store, "101", None, None, &write_only()).expect("write access alone is enough to delete");
}

#[test]
fn a_reordering_write_touches_nothing_but_is_still_a_write() {
    // Key order is data -- the model preserves it -- but it is not a path a
    // write reports as changed.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n");
    let reordered = r#"{"netbird":{"groups":["lan"]},"traefik":{"host":"a"}}"#;
    let res = put(&store, "100", None, "json", reordered, "replace", None, false, &full())
        .expect("a reordering changes no path");
    assert!(res.touched.is_empty(), "{:?}", res.touched);
    assert_eq!(
        read_raw(&store, "100").as_deref(),
        Some("netbird:\n  groups:\n  - lan\ntraefik:\n  host: a\n"),
        "the reordering is what was asked for, so it is what is stored"
    );
}

#[test]
fn a_reader_with_no_write_access_cannot_cause_a_write() {
    // The floor under every write: a reordering touches no path, so there is
    // nothing for a content check to refuse -- and without a plain
    // `acl.write` gate an auditor holding only `VM.Audit` could rewrite the
    // file.
    let (_dir, store) = store();
    let stored = "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n";
    seed(&store, "100", stored);
    let reordered = r#"{"netbird":{"groups":["lan"]},"traefik":{"host":"a"}}"#;
    let err = put(&store, "100", None, "json", reordered, "replace", None, false, &read_only())
        .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));
}

#[test]
fn a_read_403_names_the_view_it_refused() {
    // `docs/DESIGN.md` §1: key-name disclosure is out of scope, and a
    // message that says nothing is a message nobody can act on.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");
    let err = get(&store, "100", Some("netbird.groups"), "json", &none()).unwrap_err();
    assert_eq!(err.to_string(), "403: not permitted: netbird.groups", "{err}");
}

// -- the one lint -------------------------------------------------------

#[test]
fn one_lint_runs_on_the_planned_document_for_every_caller() {
    // `docs/DESIGN.md` §7: there is one lint, and it names the offending
    // path.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let before = read_raw(&store, "100").unwrap();

    for acl in [full(), write_only()] {
        for (view, mode, payload, expect) in [
            (Some("traefik__"), "replace", "5", "comment key value must be a string"),
            (Some("traefik"), "replace", "{\"bad key\": 1}", "invalid key"),
            (Some("traefik"), "replace", "{\"a.b\": 1}", "no dots"),
            (Some("traefik"), "replace", "{\"deep\": {\"bad key\": 1}}", "invalid key"),
            (Some("traefik"), "replace", "{\"list\": [{\"bad key\": 1}]}", "invalid key"),
            (Some("traefik"), "replace", "{\"x__\": 5}", "comment key value must be a string"),
            (Some("traefik"), "merge", "{\"x__\": 5}", "comment key value must be a string"),
            (Some("traefik"), "replace", "{\"nul\": null}", "null values are not allowed"),
            // A view *through* a comment key needs no rule of its own:
            // materialising `q__` as a map is what the lint refuses.
            (Some("traefik.q__.r"), "replace", "1", "comment key value must be a string"),
        ] {
            let err = put(&store, "100", view, "json", payload, mode, None, false, &acl)
                .map(|ok| panic!("{view:?}/{payload} was accepted: {ok:?}"))
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("400: "), "{payload}: {err}");
            assert!(err.contains(expect), "{payload}: {err}");
            assert_eq!(read_raw(&store, "100").unwrap(), before, "{payload} wrote anyway");
        }
    }

    // ... and a legitimate comment-key write still goes through.
    put(&store, "100", Some("traefik__"), "json", "\"the ingress config\"", "replace", None, false, &full())
        .expect("a string comment value is fine");
}

#[test]
fn the_lint_names_the_offending_path_whoever_asks() {
    // No redaction (`docs/DESIGN.md` §1): an out-of-band bad key blocks the
    // write and is spelled out.
    let (dir, store) = store();
    std::fs::write(
        dir.path().join("100.yaml"),
        "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n",
    )
    .unwrap();

    for acl in [full(), write_only()] {
        let err = put(&store, "100", Some("traefik"), "json", "{\"host\":\"y\"}", "replace", None, false, &acl)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("customer name"), "{err}");
    }
}

#[test]
fn dry_run_validates_exactly_what_the_write_validates() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");

    let dry = put(&store, "100", Some("foo__"), "json", "{\"a\": 1}", "replace", None, true, &full());
    let wet = put(&store, "100", Some("foo__"), "json", "{\"a\": 1}", "replace", None, false, &full());
    assert_eq!(status(&dry.unwrap_err()), 400);
    assert_eq!(status(&wet.unwrap_err()), 400);
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: x\n");
}

#[test]
fn dry_run_checks_the_digest_and_never_writes() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let err = put(&store, "100", Some("traefik"), "json", "{\"host\": \"y\"}", "replace", Some("deadbeef"), true, &full())
        .unwrap_err();
    assert_eq!(status(&err), 409);

    let ok = put(&store, "100", Some("traefik"), "json", "{\"host\": \"y\"}", "replace", None, true, &full())
        .unwrap();
    assert_eq!(ok.touched.len(), 1);
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: x\n");
}

// -- reads --------------------------------------------------------------

#[test]
fn a_read_without_access_is_forbidden_not_an_empty_document_with_a_real_digest() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let err = get(&store, "100", None, "json", &none()).unwrap_err();
    assert_eq!(status(&err), 403);
    assert_eq!(err.to_string(), "403: not permitted: the whole document");

    // Read access is a boolean: any caller who has it gets the same
    // whole-document digest (needed for compare-and-swap PUTs).
    let real = get(&store, "100", None, "json", &full()).unwrap();
    let read = get(&store, "100", None, "json", &read_only()).unwrap();
    assert_eq!(read.digest, real.digest);
    assert_eq!(read.data, real.data);
}

#[test]
fn a_full_read_of_the_root_view_returns_the_files_own_text() {
    let (_dir, store) = store();
    let text = "zeta: 1\nalpha__: about alpha\nalpha: 2\n";
    seed(&store, "100", text);
    assert_eq!(get(&store, "100", None, "yaml", &full()).unwrap().text.as_deref(), Some(text));
    // A sub-view is a canonical dump.
    assert_eq!(
        get(&store, "100", Some("alpha"), "yaml", &full()).unwrap().text.as_deref(),
        Some("2\n")
    );
}

// -- comment keys are notes (docs/DESIGN.md §2, §7) ---------------------

/// A read that did not ask for the notes.
fn get_bare(store: &MetaStore, id: &str, view: Option<&str>, fmt: &str, acl: &CallerAcl) -> Result<ApiViewDocument, ApiError> {
    get_document(store, id, view, fmt, false, acl)
}

/// A JSON write with `comments` as given, against `prefixes`.
#[allow(clippy::too_many_arguments)]
fn put_notes(
    store: &MetaStore,
    prefixes: &[PrefixDef],
    id: &str,
    view: Option<&str>,
    payload: &str,
    mode: &str,
    dry_run: bool,
    comments: bool,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    put_document(store, prefixes, id, view, "json", payload, mode, None, dry_run, false, comments, acl)
}

fn touched_paths(r: &ApiPutResult) -> Vec<String> {
    let mut v: Vec<String> = r.touched.iter().map(|t| format!("{} {}", t.op, t.path)).collect();
    v.sort();
    v
}

const NOTED: &str = "__: the whole document\n\
backup:\n\
\x20 __: the backup job's settings\n\
\x20 retention: 7\n\
\x20 retention__: days\n\
\x20 targets:\n\
\x20 - host: nas\n\
\x20   host__: the only one\n\
traefik__: about traefik\n\
traefik:\n\
\x20 host__: public name\n\
\x20 host: web\n\
netbird__: about netbird\n\
netbird:\n\
\x20 groups:\n\
\x20 - lan\n";

#[test]
fn a_read_carries_no_comment_key_unless_it_asks() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);
    let digest = store.digest_of(&DocId::Guest(100)).unwrap().unwrap();
    let bare = json!({
        "backup": {"retention": 7, "targets": [{"host": "nas"}]},
        "traefik": {"host": "web"},
        "netbird": {"groups": ["lan"]},
    });

    // At any depth, array members included, in both formats; YAML is the
    // canonical dump of what is left, never the file's text. The digest is the file's.
    let json_read = get_bare(&store, "100", None, "json", &full()).unwrap();
    assert_eq!((json_read.data.unwrap(), json_read.digest), (bare.clone(), digest.clone()));
    let yaml_read = get_bare(&store, "100", None, "yaml", &full()).unwrap();
    assert_eq!(
        yaml_read.text.as_deref(),
        Some("backup:\n  retention: 7\n  targets:\n  - host: nas\ntraefik:\n  host: web\nnetbird:\n  groups:\n  - lan\n")
    );
    assert_eq!(yaml_read.digest, digest);
    assert_eq!(
        get_bare(&store, "100", Some("backup"), "yaml", &full()).unwrap().text.as_deref(),
        Some("retention: 7\ntargets:\n- host: nas\n")
    );
    // Naming a note is asking for it: without `comments` that is a 400.
    for view in ["traefik__", "traefik.host__", "__"] {
        let err = get_bare(&store, "100", Some(view), "json", &full()).unwrap_err();
        assert_eq!(status(&err), 400, "{view}: {err}");
        assert!(err.msg.contains("comments=1"), "{err}");
    }

    // With `comments`, the read is what it always was: the file's own text for the
    // root view, the notes everywhere else.
    assert_eq!(get(&store, "100", None, "yaml", &full()).unwrap().text.as_deref(), Some(NOTED));
    assert_eq!(
        get(&store, "100", Some("traefik"), "json", &full()).unwrap().data.unwrap(),
        json!({"host__": "public name", "host": "web"})
    );
    assert_eq!(get(&store, "100", Some("traefik__"), "json", &full()).unwrap().data.unwrap(), json!("about traefik"));
}

#[test]
fn a_registry_document_hides_its_notes_the_same_way() {
    let (_dir, store) = store();
    seed(&store, "prefixes/traefik", "selector__: every guest for now\nselector:\n  all: true\n");
    assert_eq!(
        get_bare(&store, "prefixes/traefik", None, "json", &full()).unwrap().data.unwrap(),
        json!({"selector": {"all": true}})
    );
    assert_eq!(
        get_bare(&store, "prefixes/traefik", None, "yaml", &full()).unwrap().text.as_deref(),
        Some("selector:\n  all: true\n")
    );
    assert!(get(&store, "prefixes/traefik", None, "yaml", &full()).unwrap().text.unwrap().contains("selector__"));
}

#[test]
fn an_unrecoverable_document_shows_its_text_only_to_a_read_that_asks_for_notes() {
    let (dir, store) = store();
    std::fs::write(dir.path().join("100.yaml"), "a: [\n").unwrap();
    assert!(get(&store, "100", None, "yaml", &full()).unwrap().parse_error.is_some());
    assert_eq!(status(&get_bare(&store, "100", None, "yaml", &full()).unwrap_err()), 422);
}

#[test]
fn a_replace_without_comments_keeps_the_notes_of_what_it_keeps() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);

    // A stripped read written straight back changes nothing, touches nothing and
    // rewrites nothing.
    let bare = get_bare(&store, "100", None, "json", &full()).unwrap().data.unwrap().to_string();
    let r = put_notes(&store, &[], "100", None, &bare, "replace", false, false, &full()).unwrap();
    assert!(r.touched.is_empty(), "{:?}", r.touched);
    assert_eq!(read_raw(&store, "100").as_deref(), Some(NOTED));

    // An edit keeps every note whose subject it keeps, where it was.
    let edited = bare.replace("\"retention\":7", "\"retention\":14");
    let r = put_notes(&store, &[], "100", None, &edited, "replace", false, false, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["set backup.retention"]);
    assert_eq!(read_raw(&store, "100").unwrap(), NOTED.replace("retention: 7", "retention: 14"));

    // A note whose subject the write drops goes with it -- and says so; a map's
    // own `__` stays while the map does. A view's payload is judged the same way.
    let dry = put_notes(&store, &[], "100", Some("backup"), r#"{"targets": [{"host": "nas"}, {"host": "tape"}]}"#, "replace", true, false, &full()).unwrap();
    let r = put_notes(&store, &[], "100", Some("backup"), r#"{"targets": [{"host": "nas"}, {"host": "tape"}]}"#, "replace", false, false, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["delete backup.retention", "delete backup.retention__", "set backup.targets"]);
    assert_eq!(touched_paths(&dry), touched_paths(&r), "a dry run plans the same write");
    assert_eq!(
        get(&store, "100", Some("backup"), "json", &full()).unwrap().data.unwrap(),
        json!({"__": "the backup job's settings", "targets": [{"host": "nas", "host__": "the only one"}, {"host": "tape"}]})
    );
    // Replacing a scalar keeps the note beside it, which is outside the view anyway.
    put_notes(&store, &[], "100", Some("traefik.host"), r#""www""#, "replace", false, false, &full()).unwrap();
    assert_eq!(
        get(&store, "100", Some("traefik"), "json", &full()).unwrap().data.unwrap(),
        json!({"host__": "public name", "host": "www"})
    );
}

#[test]
fn a_replace_without_comments_may_not_carry_or_name_a_note() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);
    for (view, payload, path) in [
        (None, r#"{"traefik": {"host": "web", "host__": "new"}}"#, "traefik.host__"),
        (Some("backup"), r#"{"targets": [{"host": "nas", "host__": "x"}]}"#, "backup.targets.0.host__"),
        (Some("traefik.host__"), r#""new""#, "traefik.host__"),
    ] {
        let err = put_notes(&store, &[], "100", view, payload, "replace", false, false, &full()).unwrap_err();
        assert_eq!(status(&err), 400, "{view:?} {payload}: {err}");
        assert!(err.msg.starts_with(path), "{err}");
    }
    assert_eq!(read_raw(&store, "100").as_deref(), Some(NOTED));
}

#[test]
fn a_replace_with_comments_is_the_whole_subtree_notes_included() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);
    let r = put_notes(&store, &[], "100", Some("traefik"), r#"{"host": "web", "port__": "later"}"#, "replace", false, true, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["delete traefik.host__", "set traefik.port__"]);
    assert_eq!(
        get(&store, "100", Some("traefik"), "json", &full()).unwrap().data.unwrap(),
        json!({"host": "web", "port__": "later"})
    );
}

#[test]
fn a_merge_is_the_same_with_or_without_comments() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);
    // It names what it changes: a note it writes is written, a note it does not
    // name is left, and a key it deletes takes its note along.
    let r = put_notes(&store, &[], "100", Some("traefik"), r#"{"host__": "renamed", "port": 80}"#, "merge", false, false, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["set traefik.host__", "set traefik.port"]);
    let r = put_notes(&store, &[], "100", Some("backup"), r#"{"retention": null}"#, "merge", false, false, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["delete backup.retention", "delete backup.retention__"]);
    // ... unless the same patch says what becomes of the note.
    let r = put_notes(&store, &[], "100", Some("traefik"), r#"{"port": null, "port__": "was 80"}"#, "merge", false, true, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["delete traefik.port", "set traefik.port__"]);
}

#[test]
fn a_note_is_named_only_by_a_caller_that_asks_and_a_broken_one_says_so() {
    let (dir, store) = store();
    seed(&store, "100", NOTED);
    let rows = vec![GuestInput { vmid: 100, read: true, ..Default::default() }];
    let err = list_guests(&store, &rows, Some("traefik__")).unwrap_err();
    assert_eq!(status(&err), 400, "{err}");

    // A stored note that is not a string, from out of band: a replace that did
    // not ask for notes kept it, and the lint says what it is.
    std::fs::write(dir.path().join("100.yaml"), "a__: 5\na: 1\n").unwrap();
    let err = put_notes(&store, &[], "100", None, r#"{"a": 2}"#, "replace", false, false, &full()).unwrap_err();
    assert_eq!(status(&err), 400);
    assert!(err.msg.contains("a stored note; fix it with comments=1"), "{err}");

    // An unrecoverable file tells a reader without notes how to see it.
    std::fs::write(dir.path().join("100.yaml"), "a: [\n").unwrap();
    let err = get_bare(&store, "100", None, "yaml", &full()).unwrap_err();
    assert!(err.msg.contains("format=yaml&comments=1 (CLI: --comments)"), "{err}");
}

#[test]
fn a_delete_takes_the_note_about_what_it_removes() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);
    let r = del(&store, "100", Some("traefik"), None, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["delete traefik.host", "delete traefik.host__", "delete traefik__"]);
}

#[test]
fn a_kept_note_is_not_a_finding_under_an_enforced_schema() {
    let (_dir, store) = store();
    seed(&store, "100", NOTED);
    // Move the value away from "web" first, so writing it back is a real change.
    put_notes(&store, &[], "100", Some("traefik.host"), "\"www\"", "replace", false, false, &full()).unwrap();

    // An enforcing schema that says nothing about notes finds nothing in one kept.
    let strict = vec![registry::parse_prefix(
        "traefik",
        "selector: {all: true}\nenforce: true\nschema: {type: object, properties: {host: {type: string}}}\n",
    )
    .unwrap()];
    let r = put_notes(&store, &strict, "100", Some("traefik"), r#"{"host": "web"}"#, "replace", false, false, &full()).unwrap();
    assert_eq!(touched_paths(&r), vec!["set traefik.host"]);
    assert!(read_raw(&store, "100").unwrap().contains("host__: public name"));
}

#[test]
fn merge_with_null_deletes_end_to_end() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  spec:\n    host: a\n    port: 1\n");

    let r = put(&store, "100", Some("traefik.spec"), "json", "{\"host\": null}", "merge", None, false, &full())
        .unwrap();
    assert_eq!(r.touched.len(), 1);
    assert_eq!(r.touched[0].op, "delete");
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    port: 1\n");

    put(&store, "100", Some("traefik"), "yaml", "spec: null\n", "merge", None, false, &full()).unwrap();
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
}

#[test]
fn replace_with_an_empty_object_stores_an_empty_map() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik: {}\n");
    let r = put(&store, "100", Some("traefik"), "json", "{}", "replace", None, false, &full())
        .unwrap();
    assert!(r.touched.is_empty());
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
}

#[test]
fn get_then_put_with_the_empty_digest_creates_a_document() {
    let (_dir, store) = store();
    let got = get(&store, "999", None, "yaml", &full()).unwrap();
    assert_eq!(got.digest, "");

    let put1 = put(&store, "999", Some("traefik"), "json", "{\"host\": \"new\"}", "replace", Some(""), false, &full())
        .unwrap();
    assert!(!put1.digest.is_empty());
    assert_eq!(read_raw(&store, "999").unwrap(), "traefik:\n  host: new\n");

    let err = put(&store, "999", Some("traefik"), "json", "{\"host\": \"o\"}", "replace", Some(""), false, &full())
        .unwrap_err();
    assert_eq!(status(&err), 409);
    assert!(err.to_string().contains("<empty>"), "{err}");
}

#[test]
fn delete_of_a_view_leaves_the_rest_and_reports_touched() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");
    let r = del(&store, "100", Some("traefik"), None, &full()).unwrap();
    assert_eq!(r.touched.len(), 1);
    assert_eq!(r.touched[0].op, "delete");
    assert_eq!(read_raw(&store, "100").unwrap(), "netbird:\n  groups:\n  - lan\n");
}

// -- unparseable documents ----------------------------------------------

#[test]
fn an_unparseable_document_is_yaml_plus_parse_error_json_422_and_root_repairable() {
    // `docs/DESIGN.md` §7. This is a *per-document* condition: nothing
    // reads `datacenter.yaml` on a guest request, so it is never
    // cluster-wide.
    for broken in ["a: 1\n\tb: 2\n", "a: &x 1\nb: *x\n", "a: 1\n  b: 2\n", "a: [\n"] {
        let (dir, store) = store();
        std::fs::write(dir.path().join("100.yaml"), broken).unwrap();

        let got = get(&store, "100", None, "yaml", &full()).unwrap();
        assert_eq!(got.text.as_deref(), Some(broken), "{broken:?}");
        assert!(got.parse_error.is_some());
        assert!(!got.digest.is_empty());

        let err = get(&store, "100", None, "json", &full()).unwrap_err();
        assert_eq!(status(&err), 422, "{broken:?}: {err}");
        // Any reader gets the same 422 for `format=json`: read access is a
        // boolean, so there is no narrower structure to filter to.
        let err = get(&store, "100", None, "json", &read_only()).unwrap_err();
        assert_eq!(status(&err), 422, "{broken:?}: {err}");

        // A narrower write would plan against the empty document and drop
        // the file's content: refused.
        for (view, mode) in [(Some("traefik"), "replace"), (Some("traefik"), "merge"), (None, "merge")] {
            let err = put(&store, "100", view, "json", "{\"host\":\"y\"}", mode, None, false, &full())
                .expect_err("must be refused");
            assert_eq!(status(&err), 400, "{view:?}/{mode}: {err}");
            assert!(err.to_string().contains("repaired as a whole"), "{err}");
        }
        assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), broken);

        // The documented repair, with the compare-and-swap precondition.
        let fixed = "traefik:\n  host: y\n";
        put(&store, "100", None, "yaml", fixed, "replace", Some(&got.digest), false, &full())
            .unwrap_or_else(|e| panic!("{broken:?}: repair refused: {e}"));
        assert_eq!(read_raw(&store, "100").unwrap(), fixed);

        // ... and a root DELETE is the other repair shape.
        std::fs::write(dir.path().join("100.yaml"), broken).unwrap();
        del(&store, "100", None, None, &full()).unwrap();
        assert!(read_raw(&store, "100").is_none());
    }
}

#[test]
fn a_document_above_the_read_cap_is_refused_on_read_and_repairable_on_write() {
    let (dir, store) = store();
    let big = format!("a: \"{}\"\n", "x".repeat(4 * 1024 * 1024));
    std::fs::write(dir.path().join("100.yaml"), &big).unwrap();

    // A GET *reports* the condition rather than rendering the document
    // or 400-ing on the size: the bytes were never read, so there is no
    // `text` to hand back and every caller gets the same 422 naming the
    // two repairs.
    for fmt in ["json", "yaml"] {
        let err = get(&store, "100", None, fmt, &full()).unwrap_err();
        assert_eq!(status(&err), 422, "{fmt}: {err}");
        assert!(err.to_string().contains("too large"), "{err}");
        assert!(err.to_string().contains("mode=replace"), "{err}");
    }

    // One oversized document does not take the listing down.
    seed(&store, "101", "traefik:\n  host: x\n");
    let rows = vec![
        GuestInput { vmid: 100, read: true, ..Default::default() },
        GuestInput { vmid: 101, read: true, ..Default::default() },
    ];
    let listed = list_guests(&store, &rows, None).unwrap();
    assert_eq!(listed.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 101]);
    assert!(!listed[0].digest.is_empty(), "it still reports its real digest");

    // Nothing narrower than a whole-file replace, a root merge included.
    for (view, mode) in [(Some("traefik"), "replace"), (Some("traefik"), "merge"), (None, "merge")] {
        let err = put(&store, "100", view, "json", "{\"a\":1}", mode, None, false, &full())
            .unwrap_err();
        assert_eq!(status(&err), 400, "{view:?}/{mode}: {err}");
        assert!(err.to_string().contains("repaired as a whole"), "{err}");
    }

    let stale = put(&store, "100", None, "yaml", "a: 1\n", "replace", Some("deadbeef"), false, &full())
        .unwrap_err();
    assert_eq!(status(&stale), 409, "{stale}");
    // The digest the listing reported is the one the repair's
    // compare-and-swap accepts, even though the file was never read.
    put(&store, "100", None, "yaml", "a: 1\n", "replace", Some(&listed[0].digest), false, &full())
        .unwrap();
    assert_eq!(read_raw(&store, "100").unwrap(), "a: 1\n");

    // ... and a root DELETE is the other repair shape.
    std::fs::write(dir.path().join("100.yaml"), &big).unwrap();
    del(&store, "100", None, None, &full()).unwrap();
    assert!(!dir.path().join("100.yaml").exists());
}

#[test]
fn a_document_that_parses_to_a_non_mapping_is_repairable_only_as_a_whole() {
    // An empty or comment-only file parses *fine* — to `null` — so a
    // repair path keyed on the parse alone let a narrower write through
    // against a document with no structure to preserve. A root `merge`
    // in particular would have replaced the file wholesale while
    // reporting only the merge's own touched paths.
    for text in ["", "# just a comment\n", "- a\n- secret\n", "just a scalar\n"] {
        let (dir, store) = store();
        std::fs::write(dir.path().join("100.yaml"), text).unwrap();

        // A full reader still sees exactly what is on disk, and is told
        // why it is not a document.
        let got = get(&store, "100", None, "yaml", &full()).unwrap();
        assert_eq!(got.text.as_deref(), Some(text), "{text:?}");
        assert!(got.parse_error.is_some(), "{text:?}");
        assert!(!got.digest.is_empty(), "{text:?}");

        // Nobody else gets it rendered as an empty document ...
        for acl in [full(), read_only()] {
            let err = get(&store, "100", None, "json", &acl).unwrap_err();
            assert_eq!(status(&err), 422, "{text:?}: {err}");
            // ... and no content of it leaks in the message.
            assert!(!err.to_string().contains("secret"), "{err}");
        }

        // `?has=` cannot be used as an oracle over it either.
        let rows = vec![GuestInput { vmid: 100, read: true, ..Default::default() }];
        assert!(list_guests(&store, &rows, Some("traefik")).unwrap().is_empty());

        for (view, mode) in [(Some("traefik"), "replace"), (Some("traefik"), "merge"), (None, "merge")] {
            let err = put(&store, "100", view, "json", "{\"host\":\"y\"}", mode, None, false, &full())
                .expect_err("must be refused");
            assert_eq!(status(&err), 400, "{text:?} {view:?}/{mode}: {err}");
            assert!(err.to_string().contains("repaired as a whole"), "{err}");
        }
        assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), text);

        // The two repairs, with the compare-and-swap precondition.
        put(&store, "100", None, "yaml", "traefik:\n  host: y\n", "replace", Some(&got.digest), false, &full())
            .unwrap_or_else(|e| panic!("{text:?}: repair refused: {e}"));
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: y\n");

        std::fs::write(dir.path().join("100.yaml"), text).unwrap();
        del(&store, "100", None, None, &full()).unwrap();
        assert!(!dir.path().join("100.yaml").exists(), "{text:?}");
    }
}

#[test]
fn an_unrecoverable_document_is_repaired_only_by_a_writer() {
    // Written past the store, because a document this broken is exactly
    // what `put_raw` refuses to create: it arrived by hand, or from an
    // older writer, or from a half-finished replication.
    let (dir, store) = store();
    let broken = "traefik:\n  host: a\nhomelab: [unclosed\n";
    std::fs::write(dir.path().join("100.yaml"), broken).unwrap();
    let before = std::fs::read_to_string(dir.path().join("100.yaml")).unwrap();

    // No write access at all: refused before the content is even looked at.
    let err = put(&store, "100", None, "json", r#"{"traefik":{"host":"b"}}"#, "replace", None, false, &read_only())
        .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), before);

    // Write access repairs it, exactly as documented.
    put(&store, "100", None, "json", r#"{"traefik":{"host":"b"}}"#, "replace", None, false, &full())
        .expect("the documented repair path");
}

#[test]
fn a_write_that_changes_nothing_does_not_rewrite_the_file() {
    // The `touched: []` corner: `version()`'s `token` correctly does not
    // move for a no-op write, so `changed` must not either.
    let (dir, store) = store();
    seed(&store, "100", "traefik:\n  spec:\n    host: x\n");
    let path = dir.path().join("100.yaml");
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();

    for (view, mode, payload) in [
        (Some("traefik.spec"), "merge", "{}"),
        (Some("traefik.spec"), "merge", "{\"host\": \"x\"}"),
        (Some("traefik.spec"), "replace", "{\"host\": \"x\"}"),
        (Some("traefik.spec.gone"), "merge", "{\"nope\": null}"),
    ] {
        let r = put(&store, "100", view, "json", payload, mode, None, false, &full())
            .unwrap_or_else(|e| panic!("{view:?}/{mode}/{payload}: {e}"));
        assert!(r.touched.is_empty(), "{view:?}/{mode}/{payload}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before,
            "{view:?}/{mode}/{payload} rewrote the file"
        );
        assert_eq!(r.digest, store.digest_of(&DocId::Guest(100)).unwrap().unwrap());
    }
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    host: x\n");

    // A no-op write against a file whose *bytes* are not what we would
    // write still rewrites it: skipping is only ever a byte-for-byte
    // no-op, never a silently declined canonicalisation.
    std::fs::write(&path, "traefik:\n    spec:\n        host: x\n").unwrap();
    put(&store, "100", Some("traefik.spec"), "json", "{}", "merge", None, false, &full()).unwrap();
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    host: x\n");
}

#[test]
fn a_document_that_is_not_a_map_can_never_be_written_back() {
    // The read side of the same condition is
    // `a_document_that_parses_to_a_non_mapping_is_repairable_only_as_a_whole`;
    // this is the write gate that keeps one from being *stored*.
    let (dir, store) = store();
    for text in ["- a\n- secret\n", "just a scalar\n"] {
        std::fs::write(dir.path().join("100.yaml"), text).unwrap();
        let err = put(&store, "100", None, "yaml", text, "replace", None, false, &full()).unwrap_err();
        assert_eq!(status(&err), 400, "{text:?}: {err}");
        assert!(err.to_string().contains("top level must be an object"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), text);
    }
}

// -- listing, access, operators, gc -------------------------------------

#[test]
fn list_guests_uses_the_rows_perl_passes_and_gates_on_read_access() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    seed(&store, "200", "other:\n  k: v\n");
    let rows = vec![
        GuestInput {
            vmid: 100,
            node: Some("node1".into()),
            kind: Some("lxc".into()),
            name: Some("web".into()),
            tags: vec!["traefik".into()],
            read: true,
        },
        GuestInput {
            vmid: 200,
            node: Some("node1".into()),
            kind: Some("lxc".into()),
            name: Some("db".into()),
            read: false,
            ..Default::default()
        },
        GuestInput { vmid: 300, node: Some("node1".into()), read: true, ..Default::default() },
    ];

    // Only the guests the caller has read access on are listed, with every
    // field unconditional (`docs/DESIGN.md` §4, §8): no read, no row at all.
    let list = list_guests(&store, &rows, None).unwrap();
    assert_eq!(list.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 300]);
    assert_eq!(list[0].node.as_deref(), Some("node1"));
    assert_eq!(list[0].name.as_deref(), Some("web"));
    assert_eq!(list[0].tags, vec!["traefik".to_string()]);
    assert_eq!(list[1].digest, "", "a guest with no document reports an empty digest");

    // `has` filters on the document's data.
    let filtered = list_guests(&store, &rows, Some("traefik")).unwrap();
    assert_eq!(filtered.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100]);

    // No read access on anything: nothing is listed.
    let none_readable: Vec<GuestInput> = rows.iter().cloned().map(|mut r| { r.read = false; r }).collect();
    assert!(list_guests(&store, &none_readable, None).unwrap().is_empty());
}

#[test]
fn access_reflects_the_acl_it_is_given() {
    let (_dir, store) = store();
    let a = access(&store, &full()).unwrap();
    assert!(a.read && a.write);
    let n = access(&store, &none()).unwrap();
    assert!(!n.read && !n.write);
    let r = access(&store, &read_only()).unwrap();
    assert!(r.read && !r.write);
    let w = access(&store, &write_only()).unwrap();
    assert!(!w.read && w.write);
}

#[test]
fn prefixes_list_carries_failures_keyed_like_a_loaded_prefix() {
    let prefixes = vec![registry::parse_prefix("traefik", "selector: {all: true}\n").unwrap()];
    let failures = vec![RegistryFailure {
        name: "brokenns".to_string(),
        origin: Origin::Packaged,
        error: "missing 'selector'".to_string(),
    }];
    let ns = prefixes_list(&prefixes, &failures);
    assert_eq!(ns.len(), 2);

    let rows: Vec<serde_json::Value> = ns.iter().map(|n| serde_json::to_value(n).unwrap()).collect();
    assert_eq!(rows[0]["prefix"], json!("traefik"));
    assert!(rows[0].get("error").is_none());
    assert_eq!(rows[1]["prefix"], json!("brokenns"), "keyed like a loaded prefix -- 'prefix'");
    assert_eq!(rows[1]["origin"], json!("packaged"));
    assert_eq!(rows[1]["error"], json!("missing 'selector'"));
    assert!(rows[1].get("path").is_none(), "no filesystem path on the wire");
    assert!(rows[1].get("schema").is_none(), "a failed row has nothing a loaded one promises");
}

#[test]
fn a_document_that_vanishes_mid_request_is_404_or_absent_never_500() {
    // Reads run unlocked while writes hold `pve-meta-<id>`, so a file can
    // disappear between any two syscalls.
    let (dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let digest = get(&store, "100", None, "json", &full()).unwrap().digest;
    std::fs::remove_file(dir.path().join("100.yaml")).unwrap();

    // A read of a document that is no longer there is the empty document.
    let got = get(&store, "100", None, "json", &full()).unwrap();
    assert_eq!(got.data.unwrap(), json!({}));
    assert_eq!(got.digest, "");

    // A listing does not 500 for the whole cluster because of one of them.
    let rows = vec![GuestInput { vmid: 100, read: true, ..Default::default() }];
    let listed = list_guests(&store, &rows, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].digest, "");

    // The version poll skips it rather than failing.
    assert!(version(&store).is_ok());

    // A DELETE of a document another caller already removed is that
    // caller's request satisfied.
    let r = del(&store, "100", None, None, &full()).unwrap();
    assert_eq!(r.digest, "");
    assert!(r.touched.is_empty());

    // ... and the stale digest of the vanished document is still a
    // precondition failure, not a 500.
    let err = put(&store, "100", None, "yaml", "a: 1\n", "replace", Some(&digest), false, &full())
        .unwrap_err();
    assert_eq!(status(&err), 409, "{err}");
}

// -- registry documents ------------------------------------------------

#[test]
fn parse_id_reads_a_registry_id_and_refuses_anything_that_could_leave_the_directory() {
    assert_eq!(
        parse_id("prefixes/traefik").unwrap(),
        DocId::Registry(RegistryKind::PrefixDef, "traefik".to_string()),
    );
    // The file name *is* the prefix, so a nested prefix is a dotted file
    // name and has to be addressable: `homelab.docker.yaml` declares
    // `homelab.docker`, and refusing that id would put every nested
    // prefix out of the editor's reach.
    assert_eq!(
        parse_id("prefixes/homelab.docker").unwrap(),
        DocId::Registry(RegistryKind::PrefixDef, "homelab.docker".to_string()),
    );
    for bad in [
        "prefixes/../../etc/passwd",
        "prefixes/a/b",
        "prefixes/.hidden",
        "prefixes/",
        "prefixes/a..b",
        "prefixes/a b",
        "operators/traefik",
        // Permission files are gone (`docs/DESIGN.md` §4): access is PVE's
        // ACLs alone, so `permissions/<name>` is no longer a registry kind.
        "permissions/ops",
    ] {
        let err = parse_id(bad).unwrap_err();
        assert_eq!(status(&err), 400, "{bad} was accepted: {err}");
    }
}

#[test]
fn a_prefix_is_read_and_written_like_any_other_document() {
    let (_dir, store) = store();
    let created = put(
        &store,
        "prefixes/homelab",
        None,
        "yaml",
        "selector:\n  all: true\ndescription: Home\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap();
    assert_eq!(created.id, "prefixes/homelab");

    // A view write reaches into it like into any document, with the same
    // digest compare-and-swap.
    let doc = get(&store, "prefixes/homelab", None, "json", &full()).unwrap();
    put(
        &store,
        "prefixes/homelab",
        Some("schema.type"),
        "json",
        "\"object\"",
        "replace",
        Some(&doc.digest),
        false,
        &full(),
    )
    .unwrap();

    // And what came back out is what the loader parses -- the check that
    // matters, since a file it rejects is a file it silently skips.
    let raw = read_raw(&store, "prefixes/homelab").unwrap();
    let ns = registry::parse_prefix("homelab", &raw).unwrap();
    assert_eq!(ns.prefix.to_string(), "homelab");
    assert_eq!(ns.description.as_deref(), Some("Home"));
    assert_eq!(ns.schema.unwrap()["type"], json!("object"));
}

#[test]
fn a_registry_document_uses_only_the_acl_it_is_given() {
    let (_dir, store) = store();
    seed(&store, "prefixes/traefik", "selector: {all: true}\n");
    let err = get(&store, "prefixes/traefik", None, "yaml", &none()).unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    let err = put(&store, "prefixes/traefik", Some("description"), "json", "\"x\"", "replace", None, false, &none())
        .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
}

#[test]
fn a_write_that_would_leave_the_loader_nothing_to_read_is_refused() {
    let (_dir, store) = store();
    // No selector: `parse_prefix` refuses it, so the loader would skip
    // the file and the prefix would vanish on a 200.
    let err = put(
        &store,
        "prefixes/homelab",
        None,
        "yaml",
        "description: Home\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap_err();
    assert_eq!(status(&err), 400, "{err}");
    assert!(format!("{err}").contains("not be a valid prefix"), "{err}");
    assert_eq!(read_raw(&store, "prefixes/homelab"), None, "nothing was written");

    // A dry run is refused for the same reason, and by the same check.
    let err = put(
        &store,
        "prefixes/homelab",
        None,
        "yaml",
        "description: Home\n",
        "replace",
        Some(""),
        true,
        &full(),
    )
    .unwrap_err();
    assert_eq!(status(&err), 400, "{err}");
}

// -- a node's schema override, inside the prefix file (docs/DESIGN.md §3) ----

fn on_node(node: &str) -> CallerAcl {
    CallerAcl { node: Some(NodeName::new(node).unwrap()), ..full() }
}

/// `put_document` with the prefixes `api_put` hands it: the resolved set for
/// the caller's node and tags.
fn put_on(store: &MetaStore, id: &str, view: &str, payload: &str, acl: &CallerAcl) -> Result<ApiPutResult, ApiError> {
    let doc_id = parse_id(id).unwrap();
    let prefixes = api::effective_prefixes(store.registry().unwrap(), &doc_id, acl).unwrap();
    put_with(store, &prefixes, id, Some(view), "json", payload, "replace", None, false, false, acl)
}

#[test]
fn a_nodes_override_applies_only_to_a_guest_on_that_node() {
    let (_dir, store) = store();
    put(
        &store,
        "prefixes/gpu",
        None,
        "yaml",
        "selector: {all: true}\nenforce: true\nschema: {type: object, properties: {count: {type: integer}}}\n\
         nodes:\n  pve1: {enforce: false}\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap();

    // Enforced on pve2 (the top-level default) ...
    let err = put_on(&store, "100", "gpu.count", r#""two""#, &on_node("pve2")).unwrap_err();
    assert_eq!(status(&err), 422, "{err}");
    // ... and on a guest whose node nobody said.
    let err = put_on(&store, "101", "gpu.count", r#""two""#, &full()).unwrap_err();
    assert_eq!(status(&err), 422, "{err}");
    // ... but pve1's override turns enforcement off, whole.
    put_on(&store, "102", "gpu.count", r#""two""#, &on_node("pve1")).unwrap();
}

#[test]
fn the_resolved_prefixes_listing_carries_no_nodes_map_and_the_raw_one_does() {
    let (_dir, store) = store();
    put(
        &store,
        "prefixes/gpu",
        None,
        "yaml",
        "selector: {all: true}\nschema: {type: object}\nnodes:\n  pve1: {enforce: true}\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap();

    let raw = api::prefixes(store.registry().unwrap(), None, None).unwrap();
    let raw_row = serde_json::to_value(&raw[0]).unwrap();
    assert!(raw_row.get("nodes").is_some(), "the unresolved listing shows the override map: {raw_row:?}");

    let resolved = api::prefixes(store.registry().unwrap(), Some("pve1"), Some(&[])).unwrap();
    let resolved_row = serde_json::to_value(&resolved[0]).unwrap();
    assert!(resolved_row.get("nodes").is_none(), "a resolved row carries no nodes map: {resolved_row:?}");
    assert_eq!(resolved_row["enforce"], json!(true), "pve1's override is already applied");

    // A node name that is not one is a 400, on both forms.
    for bad in ["..", "pve1/../x", "a.b", ""] {
        assert_eq!(status(&api::prefixes(store.registry().unwrap(), Some(bad), Some(&[])).unwrap_err()), 400, "{bad:?}");
    }
}

// -- a store that is not there (docs/DESIGN.md §7) ------------------------------

#[test]
fn every_call_on_an_unavailable_store_is_a_503_never_an_empty_answer() {
    let (dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let marker = dir.path().join("local");
    let store = store.with_cluster_marker(&marker);
    let rows = vec![GuestInput { vmid: 100, read: true, ..Default::default() }];
    let unavailable = |status: u16, what: &str| assert_eq!(status, 503, "{what}");

    unavailable(get(&store, "100", None, "json", &full()).unwrap_err().status, "get");
    unavailable(get(&store, "prefixes/traefik", None, "yaml", &full()).unwrap_err().status, "get of a registry document");
    unavailable(put(&store, "100", Some("traefik.host"), "json", "\"y\"", "replace", None, true, &full()).unwrap_err().status, "dry run");
    unavailable(put(&store, "100", Some("traefik.host"), "json", "\"y\"", "replace", None, false, &full()).unwrap_err().status, "put");
    unavailable(del(&store, "100", None, None, &full()).unwrap_err().status, "delete");
    unavailable(version(&store).unwrap_err().status, "version");
    unavailable(list_guests(&store, &rows, None).unwrap_err().status, "list");
    unavailable(access(&store, &full()).unwrap_err().status, "access");
    let err = ApiError::from(store.registry().unwrap_err());
    unavailable(err.status, "the registry, for the prefix listing");
    assert!(err.msg.starts_with("cluster filesystem not available"), "{err}");
    assert_eq!(read_raw_unchecked(dir.path(), "100"), "traefik:\n  host: x\n");

    std::os::unix::fs::symlink(dir.path(), &marker).unwrap();
    assert_eq!(get(&store, "100", None, "json", &full()).unwrap().data.unwrap(), json!({"traefik": {"host": "x"}}));
    assert_eq!(list_guests(&store, &rows, None).unwrap().len(), 1);
}

fn read_raw_unchecked(root: &std::path::Path, id: &str) -> String {
    std::fs::read_to_string(root.join(format!("{id}.yaml"))).unwrap()
}
