//! Integration tests for `pve_meta_core::api`.

use pretty_assertions::assert_eq;
use pve_meta_core::api::{
    access, delete_document, effective, get_document, list_guests,
    parse_id, permissions_list, prefixes_list, put_document, version, ApiError, ApiPutResult,
    ApiViewDocument, CallerAcl, GuestInput, PermissionEntry,
};
use pve_meta_core::path::Path as DocPath;
use pve_meta_core::registry::{self, Origin, Permission, RegistryFailure};
use pve_meta_core::store::{DocId, MetaStore, RegistryKind};
use serde_json::json;

/// A store over a fresh tempdir, with its registry directories inside it
/// rather than the machine's real ones (see `tests/store.rs` for why). No
/// global state: every test owns its own root, so the suite runs in
/// parallel like the rest of the crate's.
fn store() -> (tempfile::TempDir, MetaStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MetaStore::with_registry_dirs(
        dir.path(),
        vec![dir.path().join("registry/prefixes")],
        vec![dir.path().join("registry/grants")],
    );
    (dir, store)
}

/// The grants used throughout: `scoped@pve!t1` holds `traefik` rw on
/// guests tagged `traefik`, and `netbird` ro on every guest.
fn regs() -> Vec<Permission> {
    vec![registry::parse_permission(
        "scoped",
        "authid: scoped@pve!t1\n\
         rules:\n\
         \x20 - prefix: traefik\n    mode: rw\n    selector: {tag: traefik}\n\
         \x20 - prefix: netbird\n    mode: ro\n    selector: {all: true}\n",
    )
    .unwrap()]
}

fn full() -> CallerAcl {
    CallerAcl {
        authid: "root@pam".to_string(),
        read: true,
        write: true,
        tags: vec![],
    }
}

fn scoped(tags: &[&str]) -> CallerAcl {
    CallerAcl {
        authid: "scoped@pve!t1".to_string(),
        read: false,
        write: false,
        tags: tags.iter().map(|s| s.to_string()).collect(),
    }
}

fn none() -> CallerAcl {
    CallerAcl {
        authid: "nobody@pve".to_string(),
        ..Default::default()
    }
}

/// `VM.Audit` and no `VM.Config.Options`, plus whatever scopes the
/// permission files give it: the principal every one of these rules is
/// actually about. It can read the whole document, so it can compose a
/// faithful whole-document write; what it may *change* is its scopes.
fn auditor(tags: &[&str]) -> CallerAcl {
    CallerAcl {
        authid: "scoped@pve!t1".to_string(),
        read: true,
        write: false,
        tags: tags.iter().map(|s| s.to_string()).collect(),
    }
}

/// A permission file with two `rw` rules, which is the ordinary shape the
/// old view gate could not express a write for: the narrowest view
/// covering one key in each is the document root.
fn two_rw() -> Vec<Permission> {
    vec![registry::parse_permission(
        "two",
        "authid: scoped@pve!t1\n\
         rules:\n\
         \x20 - prefix: traefik\n    mode: rw\n    selector: {all: true}\n\
         \x20 - prefix: netbird\n    mode: rw\n    selector: {all: true}\n",
    )
    .unwrap()]
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
    get_document(store, &regs(), id, view, fmt, acl)
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
    put_with(store, &regs(), id, view, fmt, payload, mode, digest, dry_run, acl)
}

#[allow(clippy::too_many_arguments)]
fn put_with(
    store: &MetaStore,
    permission_files: &[Permission],
    id: &str,
    view: Option<&str>,
    fmt: &str,
    payload: &str,
    mode: &str,
    digest: Option<&str>,
    dry_run: bool,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    put_document(store, permission_files, id, view, fmt, payload, mode, digest, dry_run, acl)
}

fn del(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    del_with(store, &regs(), id, view, digest, acl)
}

fn del_with(
    store: &MetaStore,
    permission_files: &[Permission],
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    delete_document(store, permission_files, id, view, digest, acl)
}

// -- grants from registrations ----------------------------------------

#[test]
fn version_detail_names_the_documents_that_changed() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.put_raw(&DocId::Guest(200), "b: 2\n", None).unwrap();

    // Without `detail` the shape is unchanged: no `documents` on the wire.
    let plain = version(&store, false, None).unwrap();
    assert!(plain.documents.is_none());

    let detailed = version(&store, true, None).unwrap();
    let docs = detailed.documents.expect("detail asked for");
    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, vec!["100", "200"]);
    assert_eq!(docs[0].digest, store.read(&DocId::Guest(100)).unwrap().digest);
    assert_eq!(detailed.token, plain.token, "detail does not change the token");
}

#[test]
fn a_scoped_version_ignores_other_documents_and_snapshots() {
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.put_raw(&DocId::Guest(101), "b: 1\n", None).unwrap();

    let mine = || version(&store, false, Some("100")).unwrap().token;
    let before = mine();

    // Another guest's document: the unscoped token moves, mine does not.
    let whole_before = version(&store, false, None).unwrap().token;
    store.put_raw(&DocId::Guest(101), "b: 2\n", None).unwrap();
    assert_ne!(version(&store, false, None).unwrap().token, whole_before);
    assert_eq!(mine(), before, "another guest is not my document");

    // My own snapshot copy is not my document either -- it is exactly the
    // per-guest fan-out this scoping exists to avoid.
    std::fs::write(dir.path().join("100.snap.yaml"), "a: 9\n").unwrap();
    assert_eq!(mine(), before, "a snapshot copy is not the document");

    // My own document, and only when its content actually changed.
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    assert_eq!(mine(), before, "a rewrite with the same bytes is not a change");
    store.put_raw(&DocId::Guest(100), "a: 2\n", None).unwrap();
    assert_ne!(mine(), before);
}

#[test]
fn a_scoped_version_still_watches_the_registry() {
    // The registry decides what the document *looks like* and who may
    // write it, so a scoped poll that missed it would leave an open editor
    // rendering against a schema that no longer exists.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let prefixes = dir.path().join("registry/prefixes");
    std::fs::create_dir_all(&prefixes).unwrap();

    for (id, body) in [("100", "selector: {all: true}\n"), ("prefixes/homelab", "selector: {tag: web}\n")] {
        let before = version(&store, false, Some(id)).unwrap().token;
        std::fs::write(prefixes.join("homelab.yaml"), body).unwrap();
        assert_ne!(
            version(&store, false, Some(id)).unwrap().token,
            before,
            "a prefix appearing must move the token for {id}"
        );
    }
}

#[test]
fn a_scoped_detail_lists_exactly_what_the_scoped_token_covers() {
    // `detail` answers "which of the things this token covers changed", so
    // it lists what the token is over and nothing else: this document, and
    // the registry documents -- which the scoped token watches too, because
    // they decide how the document is rendered and who may write it.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.put_raw(&DocId::Guest(101), "b: 1\n", None).unwrap();
    let prefixes = dir.path().join("registry/prefixes");
    std::fs::create_dir_all(&prefixes).unwrap();
    std::fs::write(prefixes.join("homelab.yaml"), "selector: {all: true}\n").unwrap();

    let docs = version(&store, true, Some("100")).unwrap().documents.unwrap();
    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, vec!["100", "prefixes/homelab"]);
}

#[test]
fn a_scoped_version_refuses_a_garbage_id() {
    // The same parser every other endpoint uses: a 400, not a 500 and not
    // a silent fall back to the whole store.
    let (_dir, store) = store();
    let err = version(&store, false, Some("nope")).unwrap_err().to_string();
    assert!(err.starts_with("400: "), "{err}");
}

#[test]
    fn a_selector_resolves_against_the_guests_tags() {
    // `docs/DESIGN.md` §3: adding the tag is the deliberate act of
    // granting the operator that guest.
    let permission_files = regs();
    let untagged = effective(&permission_files, &DocId::Guest(100), &scoped(&[]));
    assert_eq!(untagged.scopes.len(), 1);
    assert_eq!(untagged.scopes[0].prefix.to_string(), "netbird");
    assert!(!untagged.can_write(&DocPath::parse("traefik").unwrap()));

    let tagged = effective(&permission_files, &DocId::Guest(100), &scoped(&["traefik"]));
    assert!(tagged.can_write(&DocPath::parse("traefik.spec").unwrap()));
    assert!(tagged.can_read(&DocPath::parse("netbird").unwrap()));
    assert!(!tagged.can_write(&DocPath::parse("netbird").unwrap()));
}

#[test]
fn a_registration_for_another_authid_permissions_nothing() {
    let g = effective(&regs(), &DocId::Guest(100), &none());
    assert!(g.readable_prefixes().is_empty());
}

// -- write authorization ------------------------------------------------

#[test]
fn zero_permission_token_cannot_create_structure_through_an_empty_merge() {
    // A `PUT ?view=zzz.deep&mode=merge` with `{}` must not write
    // `zzz: {deep: {}}` while reporting `touched: []`: `check_write([])`
    // is vacuously Ok, so the up-front view check is what stops it.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  spec:\n    host: ct100.example\n");
    let before = read_raw(&store, "100").unwrap();

    for acl in [none(), scoped(&["traefik"])] {
        for (view, mode, payload) in [
            (Some("zzz_hacked.deep"), "merge", "{}"),
            (Some("zzz_hacked"), "merge", "{}"),
            (Some("zzz_hacked.deep"), "replace", "{}"),
        ] {
            let err = put(&store, "100", view, "json", payload, mode, None, false, &acl)
                .expect_err("must be refused");
            assert_eq!(status(&err), 403, "{view:?}/{mode}: {err}");
            assert_eq!(read_raw(&store, "100").unwrap(), before, "{view:?}/{mode} mutated");
        }
    }
}

/// Still true after the gate moved to the content, but for two different
/// reasons: a token with nothing has no write permission at all, and a
/// scope-only one cannot read the whole document it would be replacing.
#[test]
fn a_token_without_full_read_cannot_write_the_root_view() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let before = read_raw(&store, "100").unwrap();
    for acl in [none(), scoped(&["traefik"])] {
        for (mode, payload) in [("merge", "{}"), ("replace", "{\"a\": 1}")] {
            let err = put(&store, "100", None, "json", payload, mode, None, false, &acl)
                .expect_err("neither principal can name the root view");
            assert_eq!(status(&err), 403, "{mode}: {err}");
        }
        let err = del(&store, "100", None, None, &acl).expect_err("root delete");
        assert_eq!(status(&err), 403, "{err}");
    }
    assert_eq!(read_raw(&store, "100").unwrap(), before);
}

// -- what authorizes a write is what it changes -----------------------
//
// `docs/DESIGN.md` §3.4. The gate used to be the *view*: it had to sit
// inside a writable scope, and the root view took full write access. That
// refused writes which violated nobody's permissions, so these fix the
// rule at the level it is actually about.

#[test]
fn one_write_may_span_two_granted_prefixes() {
    // A permission file has `rules`, plural. Editing one key in each of
    // two `rw` prefixes is ordinary, and no view but the root covers both.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n");
    let doc = r#"{"traefik":{"host":"b"},"netbird":{"groups":["wan"]}}"#;
    let res = put_with(
        &store, &two_rw(), "100", None, "json", doc, "replace", None, false, &auditor(&[]),
    )
    .expect("every change is inside a granted prefix");
    assert_eq!(
        res.touched.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(),
        vec!["traefik.host", "netbird.groups"]
    );
}

#[test]
fn a_whole_document_write_still_answers_for_every_key_it_changes() {
    // The permissiveness is exactly "every change is permitted" and not one
    // step further: the same root replace that is allowed above is refused
    // the moment it reaches outside the scopes -- by changing a key, and by
    // quietly dropping one, which is the shape that would lose data.
    let (_dir, store) = store();
    let stored = "traefik:\n  host: a\nhomelab:\n  owner: arki\n";
    for (label, doc) in [
        ("changes an outside key", r#"{"traefik":{"host":"b"},"homelab":{"owner":"mallory"}}"#),
        ("drops an outside key", r#"{"traefik":{"host":"b"}}"#),
        ("adds an outside key", r#"{"traefik":{"host":"a"},"homelab":{"owner":"arki"},"new":1}"#),
    ] {
        seed(&store, "100", stored);
        let err = put_with(
            &store, &two_rw(), "100", None, "json", doc, "replace", None, false, &auditor(&[]),
        )
        .unwrap_err();
        assert_eq!(status(&err), 403, "{label}: {err}");
        assert!(err.to_string().contains("homelab") || err.to_string().contains("new"), "{label}: {err}");
        assert_eq!(read_raw(&store, "100").as_deref(), Some(stored), "{label} mutated");
    }
}

#[test]
fn reordering_keys_is_a_write_a_scoped_principal_may_make() {
    // Key order is data -- the model preserves it -- but it is not a path,
    // so a reordering changes nothing the permission rules are written
    // about. It can only be expressed as a whole-document write, which is
    // why it used to be a 403 for anyone without full write access.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n");
    let reordered = r#"{"netbird":{"groups":["lan"]},"traefik":{"host":"a"}}"#;
    let res = put_with(
        &store, &two_rw(), "100", None, "json", reordered, "replace", None, false,
        &auditor(&[]),
    )
    .expect("a reordering changes no path");
    assert!(res.touched.is_empty(), "{:?}", res.touched);
    assert_eq!(
        read_raw(&store, "100").as_deref(),
        Some("netbird:\n  groups:\n  - lan\ntraefik:\n  host: a\n"),
        "the reordering is what was asked for, so it is what is stored"
    );
}

#[test]
fn a_reader_with_no_write_permission_cannot_cause_a_write() {
    // The floor under the content check. A reordering touches no path, so
    // the content check has nothing to refuse -- and without this, an
    // auditor holding nothing but `VM.Audit` could rewrite the file.
    let (_dir, store) = store();
    let stored = "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n";
    seed(&store, "100", stored);
    let reordered = r#"{"netbird":{"groups":["lan"]},"traefik":{"host":"a"}}"#;
    let readonly = CallerAcl { authid: "auditor@pve".into(), read: true, ..Default::default() };
    let err = put(&store, "100", None, "json", reordered, "replace", None, false, &readonly)
        .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));

    // A read-only *scope* is not write permission either.
    let ro_only = vec![registry::parse_permission(
        "ro",
        "authid: auditor@pve\nrules:\n\x20 - prefix: netbird\n    mode: ro\n    selector: {all: true}\n",
    )
    .unwrap()];
    let err = put_with(
        &store, &ro_only, "100", None, "json", reordered, "replace", None, false, &readonly,
    )
    .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));
}

#[test]
fn a_write_you_may_not_read_is_refused_before_it_can_answer_anything() {
    // Without the read requirement the content check is a read oracle:
    // replace a key you cannot read with a guess, and 200-versus-403 tells
    // you whether the guess was right, one guess at a time.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: a\nsecret:\n  token: hunter2\n");
    // `two_rw` grants traefik and netbird; `secret` is neither, and this
    // principal has no full read.
    for guess in ["\"hunter2\"", "\"wrong\""] {
        let err = put_with(
            &store, &two_rw(), "100", Some("secret.token"), "json", guess, "replace", None,
            false, &scoped(&[]),
        )
        .unwrap_err();
        assert_eq!(status(&err), 403, "{guess}: {err}");
        // The refusal must not depend on the guess, or it is the oracle again.
        assert_eq!(err.to_string(), "403: not permitted: secret.token", "{guess}: {err}");
    }
}

#[test]
fn an_unreadable_document_is_repaired_only_by_full_write() {
    // The one place the coarse rule survives, because the diff is blind
    // here: the stored value is the empty document, so a scoped principal
    // replacing the root with its own subtree produces a touched list
    // entirely inside its own scope -- while destroying every other
    // prefix's content in a file nobody can currently read.
    // Written past the store, because a document this broken is exactly
    // what `put_raw` refuses to create: it arrived by hand, or from an
    // older writer, or from a half-finished replication.
    let (dir, store) = store();
    let broken = "traefik:\n  host: a\nhomelab: [unclosed\n";
    std::fs::write(dir.path().join("100.yaml"), broken).unwrap();
    let before = std::fs::read_to_string(dir.path().join("100.yaml")).unwrap();
    let err = put_with(
        &store, &two_rw(), "100", None, "json", r#"{"traefik":{"host":"b"}}"#, "replace",
        None, false, &auditor(&[]),
    )
    .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    assert!(err.to_string().contains("cannot be read back"), "{err}");
    assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), before);

    // Full write access still repairs it, exactly as documented.
    put(&store, "100", None, "json", r#"{"traefik":{"host":"b"}}"#, "replace", None, false, &full())
        .expect("the documented repair path");
}

#[test]
fn a_root_delete_still_answers_for_everything_it_removes() {
    let (_dir, store) = store();
    let stored = "traefik:\n  host: a\nhomelab:\n  owner: arki\n";
    seed(&store, "100", stored);
    let err = del_with(&store, &two_rw(), "100", None, None, &auditor(&[])).unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    assert!(err.to_string().contains("homelab"), "{err}");
    assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));
}

#[test]
fn a_403_names_the_path_it_refused() {
    // `docs/DESIGN.md` §1: key-name disclosure is out of scope, and a
    // message that says nothing is a message nobody can act on.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");
    let err = put(
        &store,
        "100",
        Some("netbird.groups"),
        "json",
        "[\"guess\"]",
        "replace",
        None,
        false,
        &scoped(&["traefik"]),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "403: not permitted: netbird.groups", "{err}");
}

#[test]
fn a_scoped_write_outside_the_view_is_refused_by_the_touched_check() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    // A root merge is refused by the view gate; a *scoped* merge whose
    // patch reaches out of the prefix cannot exist (the patch is applied
    // relative to the view), so the touched check is exercised through a
    // full-read/no-write caller instead.
    let ro = CallerAcl { authid: "ro@pve".into(), read: true, write: false, tags: vec![] };
    let err = put(&store, "100", Some("traefik"), "json", "{\"a\":1}", "replace", None, false, &ro)
        .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
}

// -- the one lint -------------------------------------------------------

#[test]
fn one_lint_runs_on_the_planned_document_for_every_caller() {
    // `docs/DESIGN.md` §4. Revision 4 narrowed the lint by privilege and
    // then needed a finding-set subset check to make the narrowing safe;
    // there is one lint now, and it names the offending path.
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let before = read_raw(&store, "100").unwrap();

    for acl in [full(), scoped(&["traefik"])] {
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
    put(&store, "100", Some("traefik__"), "json", "\"the ingress config\"", "replace", None, false, &scoped(&["traefik"]))
        .expect("a string comment value is fine");
}

#[test]
fn the_lint_names_the_offending_path_whoever_asks() {
    // No redaction (`docs/DESIGN.md` §1, §10): an out-of-band bad key
    // blocks the write and is spelled out, for a scoped caller too.
    let (dir, store) = store();
    std::fs::write(
        dir.path().join("100.yaml"),
        "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n",
    )
    .unwrap();

    for acl in [full(), scoped(&["traefik"])] {
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
fn no_permission_read_is_forbidden_not_an_empty_document_with_a_real_digest() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  host: x\n");
    let err = get(&store, "100", None, "json", &none()).unwrap_err();
    assert_eq!(status(&err), 403);
    assert_eq!(err.to_string(), "403: not permitted: the whole document");

    // A partial grant still gets the whole-document digest (needed for
    // compare-and-swap PUTs).
    let real = get(&store, "100", None, "json", &full()).unwrap();
    let partial = get(&store, "100", None, "json", &scoped(&["traefik"])).unwrap();
    assert_eq!(partial.digest, real.digest);
}

#[test]
fn a_scoped_read_sees_only_its_own_prefixes() {
    let (_dir, store) = store();
    seed(
        &store,
        "100",
        "__: top level note\ntraefik__: about traefik\ntraefik:\n  host: x\nnetbird:\n  groups:\n  - lan\nother: 1\n",
    );

    let got = get(&store, "100", None, "json", &scoped(&["traefik"])).unwrap();
    assert_eq!(
        got.data.unwrap(),
        json!({"traefik__": "about traefik", "traefik": {"host": "x"},
               "netbird": {"groups": ["lan"]}})
    );

    // The bare `__` documents the whole document and is not disclosed;
    // the explicit view of it agrees.
    let err = get(&store, "100", Some("__"), "json", &scoped(&["traefik"])).unwrap_err();
    assert_eq!(status(&err), 403, "{err}");

    // Untag the guest and the traefik scope disappears from both.
    let untagged = get(&store, "100", None, "json", &scoped(&[])).unwrap();
    assert_eq!(untagged.data.unwrap(), json!({"netbird": {"groups": ["lan"]}}));
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

#[test]
fn merge_with_null_deletes_end_to_end() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik:\n  spec:\n    host: a\n    port: 1\n");
    let acl = scoped(&["traefik"]);

    let r = put(&store, "100", Some("traefik.spec"), "json", "{\"host\": null}", "merge", None, false, &acl)
        .unwrap();
    assert_eq!(r.touched.len(), 1);
    assert_eq!(r.touched[0].op, "delete");
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    port: 1\n");

    put(&store, "100", Some("traefik"), "yaml", "spec: null\n", "merge", None, false, &acl).unwrap();
    assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
}

#[test]
fn replace_with_an_empty_object_stores_an_empty_map() {
    let (_dir, store) = store();
    seed(&store, "100", "traefik: {}\n");
    let r = put(&store, "100", Some("traefik"), "json", "{}", "replace", None, false, &scoped(&["traefik"]))
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
    let r = del(&store, "100", Some("traefik"), None, &scoped(&["traefik"])).unwrap();
    assert_eq!(r.touched.len(), 1);
    assert_eq!(r.touched[0].op, "delete");
    assert_eq!(read_raw(&store, "100").unwrap(), "netbird:\n  groups:\n  - lan\n");
}

// -- unparseable documents ----------------------------------------------

#[test]
fn an_unparseable_document_is_yaml_plus_parse_error_json_422_and_root_repairable() {
    // `docs/DESIGN.md` §4. This is a *per-document* condition: nothing
    // reads `datacenter.yaml` on a guest request any more, so it is never
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
        // A scoped reader gets 422 either way: there is no structure to
        // filter, and the bytes are not theirs to repair.
        let err = get(&store, "100", None, "yaml", &scoped(&["traefik"])).unwrap_err();
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
    let listed = list_guests(&store, &regs(), "root@pam", &rows, None).unwrap();
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
        for acl in [full(), scoped(&["traefik"])] {
            let err = get(&store, "100", None, "json", &acl).unwrap_err();
            assert_eq!(status(&err), 422, "{text:?}: {err}");
            // ... and no content of it leaks in the message.
            assert!(!err.to_string().contains("secret"), "{err}");
        }

        // `?has=` cannot be used as an oracle over it either.
        let rows = vec![GuestInput { vmid: 100, ..Default::default() }];
        assert!(list_guests(&store, &regs(), "scoped@pve!t1", &rows, Some("traefik"))
            .unwrap()
            .is_empty());

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
fn list_guests_uses_the_rows_perl_passes_and_gates_node_name_and_tags() {
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
            ..Default::default()
        },
        GuestInput {
            vmid: 200,
            node: Some("node1".into()),
            kind: Some("lxc".into()),
            name: Some("db".into()),
            ..Default::default()
        },
        GuestInput { vmid: 300, node: Some("node1".into()), ..Default::default() },
    ];

    // The scoped caller: 100 and 200 both carry the `netbird` all-guests
    // scope, so both are listed; 300 too (the scope applies to it as
    // well, it just has no document).
    let list = list_guests(&store, &regs(), "scoped@pve!t1", &rows, None).unwrap();
    assert_eq!(list.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 200, 300]);
    assert!(list[0].node.is_none() && list[0].name.is_none() && list[0].tags.is_none());

    // A principal with no ACL and no registration sees nothing.
    assert!(list_guests(&store, &regs(), "nobody@pve", &rows, None).unwrap().is_empty());

    // With VM.Audit, node, name and tags come through.
    let audited: Vec<GuestInput> =
        rows.iter().cloned().map(|mut r| { r.read = true; r }).collect();
    let list2 = list_guests(&store, &regs(), "root@pam", &audited, None).unwrap();
    assert_eq!(list2[0].node.as_deref(), Some("node1"));
    assert_eq!(list2[0].name.as_deref(), Some("web"));
    assert_eq!(list2[0].tags.as_deref(), Some(&["traefik".to_string()][..]));
    assert_eq!(list2[2].digest, "", "a guest with no document reports an empty digest");

    // `has` filters on the *visible* data.
    let filtered = list_guests(&store, &regs(), "root@pam", &audited, Some("traefik")).unwrap();
    assert_eq!(filtered.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100]);
    // ... and cannot see through a caller's own missing scope.
    assert!(list_guests(&store, &regs(), "scoped@pve!t1", &rows, Some("other"))
        .unwrap()
        .is_empty());
}

#[test]
fn access_reports_resolved_scopes() {
    let permission_files = regs();
    let tagged = access(&permission_files, &DocId::Guest(100), &scoped(&["traefik"]));
    assert!(!tagged.read && !tagged.write);
    assert_eq!(
        tagged.scopes.iter().map(|s| s.prefix.to_string()).collect::<Vec<_>>(),
        vec!["traefik", "netbird"]
    );
    let untagged = access(&permission_files, &DocId::Guest(100), &scoped(&[]));
    assert_eq!(untagged.scopes.len(), 1);
    // A registry document gets the ACL answers through and never a scope.
    let reg = access(&permission_files, &parse_id("prefixes/traefik").unwrap(), &full());
    assert!(reg.read && reg.write && reg.scopes.is_empty());
}

#[test]
fn access_returns_the_tags_only_to_a_caller_who_may_read_the_guest() {
    // This field exists so the editor stops calling `GET /meta/guests`
    // just to learn one guest's tags. It must therefore be filtered the
    // way that endpoint filters the same field, or moving it would have
    // widened who can see a guest's tags.
    let permission_files = regs();

    let mut auditor = full();
    auditor.tags = vec!["traefik".to_string()];
    let seen = access(&permission_files, &DocId::Guest(100), &auditor);
    assert_eq!(seen.tags, vec!["traefik".to_string()]);

    // A scope-only principal has no VM.Audit: it gets the scopes its
    // permission file grants -- resolved against those very tags -- but
    // not the tags themselves.
    let hidden = access(&permission_files, &DocId::Guest(100), &scoped(&["traefik"]));
    assert!(hidden.tags.is_empty(), "no VM.Audit, no tags");
    assert_eq!(hidden.scopes.len(), 2, "the selector still resolved server-side");

    assert!(access(&permission_files, &parse_id("prefixes/traefik").unwrap(), &full()).tags.is_empty());
}

#[test]
fn permissions_list_returns_every_permission() {
    let gs = permissions_list(&regs(), &[]);
    assert_eq!(gs.len(), 1);
    match &gs[0] {
        PermissionEntry::Loaded(g) => {
            assert_eq!(g.authid, "scoped@pve!t1");
            assert_eq!(g.rules.len(), 2);
        }
        PermissionEntry::Failed(_) => panic!("regs() has no failures"),
    }
}

/// The one place a file that did not load is visible at all (`docs/DESIGN.md`
/// §1 and the module docs on `registry::RegistryFailure`): the listing has to
/// carry both a loaded entry and a failed one, and the failed one has to be
/// keyed the same way a loaded row is, or a consumer reading both arrays has
/// no field to find either by.
#[test]
fn permissions_list_carries_failures_keyed_like_a_loaded_permission() {
    let failures = vec![RegistryFailure {
        name: "broken".to_string(),
        origin: Origin::Cluster,
        error: "bad mode".to_string(),
    }];
    let gs = permissions_list(&regs(), &failures);
    assert_eq!(gs.len(), 2);

    let rows: Vec<serde_json::Value> =
        gs.iter().map(|g| serde_json::to_value(g).unwrap()).collect();
    assert_eq!(rows[0]["name"], json!("scoped"));
    assert!(rows[0].get("error").is_none(), "a loaded row has no error");
    assert_eq!(rows[1]["name"], json!("broken"), "keyed like a loaded permission -- 'name'");
    assert_eq!(rows[1]["origin"], json!("cluster"));
    assert_eq!(rows[1]["error"], json!("bad mode"));
    assert!(rows[1].get("path").is_none(), "no filesystem path on the wire");
    assert!(rows[1].get("authid").is_none(), "a failed row has nothing a loaded one promises");
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
    // Reads run unlocked while writes hold `pve-meta-<id>` and the GC
    // holds `pve-meta-gc`, so a file can disappear between any two
    // syscalls. Every one of these used to be an `Error::Io` → 500.
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
    let listed = list_guests(&store, &regs(), "root@pam", &rows, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].digest, "");

    // The version poll skips it rather than failing.
    assert!(version(&store, false, None).is_ok());
    assert!(version(&store, true, None).is_ok());

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
    assert_eq!(
        parse_id("permissions/scoped").unwrap(),
        DocId::Registry(RegistryKind::Permission, "scoped".to_string()),
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

    // An authid that is not an authid is refused on the permission side.
    let err = put(
        &store,
        "permissions/ops",
        None,
        "yaml",
        "authid: not-an-authid\nrules: []\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap_err();
    assert_eq!(status(&err), 400, "{err}");
    assert!(format!("{err}").contains("not be a valid permission file"), "{err}");
}

#[test]
fn a_partial_delete_that_would_break_a_permission_file_is_refused() {
    let (_dir, store) = store();
    put(
        &store,
        "permissions/ops",
        None,
        "yaml",
        "authid: ops@pve!t1\nrules:\n  - prefix: homelab\n    mode: rw\n    selector: {all: true}\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap();

    let err = del(&store, "permissions/ops", Some("authid"), None, &full()).unwrap_err();
    assert_eq!(status(&err), 400, "{err}");
    let raw = read_raw(&store, "permissions/ops").unwrap();
    assert!(registry::parse_permission("ops", &raw).is_ok(), "the file still loads");

    // Removing the file whole is fine: that is an administrator revoking a
    // grant, not a half-written one.
    del(&store, "permissions/ops", None, None, &full()).unwrap();
    assert_eq!(read_raw(&store, "permissions/ops"), None);
}

#[test]
fn a_permission_never_reaches_the_registry_documents() {
    let (_dir, store) = store();
    put(
        &store,
        "prefixes/traefik",
        None,
        "yaml",
        "selector: {all: true}\n",
        "replace",
        Some(""),
        false,
        &full(),
    )
    .unwrap();

    // `scoped@pve!t1` holds `traefik` rw -- on *guests*. A registry
    // document gets no scopes at all.
    let acl = scoped(&["traefik"]);
    let id = parse_id("prefixes/traefik").unwrap();
    assert!(effective(&regs(), &id, &acl).scopes.is_empty());

    // `access` passes the ACL answers through untouched for these documents --
    // a registry file is readable by every authenticated user, while writing
    // one is Sys.Modify -- so the caller has to say which document it is asking
    // about, and `GET /meta/access?id=` is that question.
    let admin = CallerAcl {
        authid: "writer@pve".to_string(),
        read: true,
        write: true,
        tags: vec![],
    };
    let a = access(&regs(), &id, &admin);
    assert!(a.read && a.write && a.scopes.is_empty());
    let nobody = access(&regs(), &id, &none());
    assert!(!nobody.read && !nobody.write);
    let err = get(&store, "prefixes/traefik", None, "yaml", &acl).unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
    let err = put(
        &store,
        "prefixes/traefik",
        Some("traefik"),
        "json",
        "1",
        "replace",
        None,
        false,
        &acl,
    )
    .unwrap_err();
    assert_eq!(status(&err), 403, "{err}");
}
