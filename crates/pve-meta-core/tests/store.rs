//! Integration tests for `pve_meta_core::store::MetaStore`.

use pretty_assertions::assert_eq;
use pve_meta_core::digest::digest;
use pve_meta_core::error::Error;
use pve_meta_core::store::{DocId, MetaStore, RollbackOutcome, MAX_READ_BYTES};
use serde_json::{json, Value};
use tempfile::tempdir;

fn store() -> (tempfile::TempDir, MetaStore) {
    let dir = tempdir().unwrap();
    let store = MetaStore::new(dir.path());
    (dir, store)
}

#[test]
fn read_missing_is_not_found() {
    let (_dir, store) = store();
    let err = store.read(DocId::Guest(100)).unwrap_err();
    assert!(matches!(err, Error::NotFound(DocId::Guest(100))));
}

#[test]
fn put_raw_creates_and_replaces_and_diffs() {
    let (_dir, store) = store();
    let result = store.put_raw(DocId::Guest(100), "a: 1\nb: 2\n", None).unwrap();
    assert_eq!(result.document.value, json!({"a": 1, "b": 2}));
    assert!(result.document.path.ends_with("100.yaml"));
    // diff against an empty starting document
    assert_eq!(result.touched.len(), 2);

    let result2 = store
        .put_raw(DocId::Guest(100), "a: 10\nc: 3\n", Some(&result.document.digest))
        .unwrap();
    assert_eq!(result2.document.value, json!({"a": 10, "c": 3}));
    let mut touched: Vec<String> = result2.touched.iter().map(|t| t.path.to_string()).collect();
    touched.sort();
    assert_eq!(touched, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
}

#[test]
fn put_raw_digest_mismatch() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let err = store
        .put_raw(DocId::Guest(100), "a: 2\n", Some("nope"))
        .unwrap_err();
    assert!(matches!(err, Error::DigestMismatch { .. }));
}

#[test]
fn put_raw_empty_digest_matches_a_missing_document() {
    // `GET` reports digest "" for a non-existent document, so
    // the documented GET-then-PUT-with-digest create flow must work.
    let (_dir, store) = store();
    let result = store.put_raw(DocId::Guest(100), "a: 1\n", Some("")).unwrap();
    assert_eq!(result.document.value, json!({"a": 1}));

    // Once it exists, "" no longer matches.
    let err = store
        .put_raw(DocId::Guest(100), "a: 2\n", Some(""))
        .unwrap_err();
    match err {
        Error::DigestMismatch { expected, actual } => {
            assert_eq!(expected, "");
            assert_eq!(actual, result.document.digest);
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
}

#[test]
fn put_raw_non_empty_digest_against_a_missing_document_is_a_mismatch() {
    let (_dir, store) = store();
    let err = store
        .put_raw(DocId::Guest(100), "a: 1\n", Some("deadbeef"))
        .unwrap_err();
    match err {
        Error::DigestMismatch { expected, actual } => {
            assert_eq!(expected, "deadbeef");
            assert_eq!(actual, "");
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
}

#[test]
fn put_raw_rejects_invalid_yaml_and_invalid_documents() {
    let (_dir, store) = store();
    assert!(matches!(
        store.put_raw(DocId::Guest(100), "a: [\n", None),
        Err(Error::Parse { .. })
    ));
    assert!(matches!(
        store.put_raw(DocId::Guest(100), "a: ~\n", None),
        Err(Error::Lint(_))
    ));
    assert!(matches!(
        store.put_raw(DocId::Guest(100), "- 1\n- 2\n", None),
        Err(Error::Lint(_))
    ));
    // Nothing was written.
    assert!(store.locate(DocId::Guest(100)).unwrap().is_none());
}

#[test]
fn put_raw_normalizes_the_trailing_newline_and_digest_matches_the_file() {
    let (_dir, store) = store();
    let r = store.put_raw(DocId::Guest(100), "a: 1", None).unwrap();
    assert_eq!(r.document.raw, "a: 1\n");
    let on_disk = std::fs::read(&r.document.path).unwrap();
    assert_eq!(digest(&on_disk), r.document.digest);
}

#[test]
fn delete_removes_document() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.delete(DocId::Guest(100)).unwrap();
    assert!(matches!(store.read(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn delete_missing_is_not_found() {
    let (_dir, store) = store();
    assert!(matches!(store.delete(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn delete_leaves_snapshots_alone() {
    // `DELETE /meta/guests/{vmid}` has no concept of snapshots, so `delete`
    // must never cascade into them. Only `purge` (the GC) does.
    let (dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "snapA").unwrap();
    store.delete(DocId::Guest(100)).unwrap();
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["snapA".to_string()]);
    assert!(dir.path().join("100.snapA.yaml").exists());
}

#[test]
fn purge_removes_the_document_and_every_snapshot() {
    let (dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "snapA").unwrap();
    store.snapshot(100, "snapB").unwrap();
    assert_eq!(store.purge(100).unwrap(), 3);
    assert!(matches!(store.read(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
    assert!(store.list_snapshots(100).unwrap().is_empty());
    assert!(!dir.path().join("100.snapA.yaml").exists());
    assert!(!dir.path().join("100.snapB.yaml").exists());
}

#[test]
fn purge_is_idempotent_and_cleans_left_behind_snapshots() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "leftover").unwrap();
    store.delete(DocId::Guest(100)).unwrap();
    // No live document, but a snapshot survives the API delete.
    assert_eq!(store.purge(100).unwrap(), 1);
    assert!(store.list_snapshots(100).unwrap().is_empty());
    assert_eq!(store.purge(100).unwrap(), 0);
}

#[test]
fn api_delete_then_rollback_does_not_lose_snapshot_metadata() {
    // The forward propagation of that rule: a plain DELETE that ate the
    // snapshot copies, after which `on_rollback` read "no snapshot" as
    // "there was no metadata" and deleted the freshly rewritten document.
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "traefik:\n  host: a\n", None).unwrap();
    store.snapshot(100, "snapA").unwrap();

    // A user empties the document through the API...
    store.delete(DocId::Guest(100)).unwrap();
    // ... then writes new metadata ...
    store.put_raw(DocId::Guest(100), "traefik:\n  host: b\n", None).unwrap();
    // ... then rolls the guest back to snapA.
    assert_eq!(store.rollback(100, "snapA").unwrap(), RollbackOutcome::Restored);
    assert_eq!(
        store.read(DocId::Guest(100)).unwrap().value,
        json!({"traefik": {"host": "a"}})
    );
}

#[test]
fn snapshot_rollback_delete_and_list() {
    let (_dir, store) = store();
    // snapshot with no document is a no-op
    assert!(!store.snapshot(100, "none").unwrap());

    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    assert!(store.snapshot(100, "v1").unwrap());
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["v1".to_string()]);

    let d = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    assert!(store.snapshot(100, "v2").unwrap());
    assert_eq!(
        store.list_snapshots(100).unwrap(),
        vec!["v1".to_string(), "v2".to_string()]
    );

    // rollback restores content
    let outcome = store.rollback(100, "v1").unwrap();
    assert_eq!(outcome, RollbackOutcome::Restored);
    assert_eq!(store.read(DocId::Guest(100)).unwrap().value, json!({"a": 1}));

    store.delete_snapshot(100, "v2").unwrap();
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["v1".to_string()]);

    // deleting a missing snapshot is idempotent
    store.delete_snapshot(100, "v2").unwrap();
}

#[test]
fn snapshot_re_snapshot_overwrites() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "v1").unwrap();
    let d = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    store.snapshot(100, "v1").unwrap();
    store.rollback(100, "v1").unwrap();
    assert_eq!(store.read(DocId::Guest(100)).unwrap().value, json!({"a": 2}));
}

#[test]
fn rollback_without_snapshot_but_with_live_doc_removes_it() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let outcome = store.rollback(100, "never-existed").unwrap();
    assert_eq!(outcome, RollbackOutcome::RemovedNoSnapshot);
    assert!(matches!(store.read(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn rollback_with_neither_snapshot_nor_doc_is_noop() {
    let (_dir, store) = store();
    let outcome = store.rollback(100, "never-existed").unwrap();
    assert_eq!(outcome, RollbackOutcome::NoOp);
}

#[test]
fn rollback_rejects_invalid_snapshot_name() {
    let (_dir, store) = store();
    let err = store.rollback(100, "1bad").unwrap_err();
    assert!(matches!(err, Error::InvalidName(_)));
}

#[test]
fn list_snapshots_ignores_documents_temp_files_and_other_guests() {
    let (dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.put_raw(DocId::Guest(1000), "a: 1\n", None).unwrap();
    store.put_raw(DocId::Datacenter, "a: 1\n", None).unwrap();
    store.snapshot(100, "before").unwrap();
    std::fs::write(dir.path().join(".100.yaml.tmp.node1.42.0"), "junk").unwrap();
    std::fs::write(dir.path().join("100.bad name.yaml"), "a: 1\n").unwrap();

    assert_eq!(store.list_snapshots(100).unwrap(), vec!["before".to_string()]);
    assert!(store.list_snapshots(1000).unwrap().is_empty());
}

#[test]
fn write_atomic_leaves_no_temp_files_behind() {
    let (dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let d = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
}

#[test]
fn version_token_changes_on_write_not_on_read() {
    let (_dir, store) = store();
    let v0 = store.version().unwrap();

    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    assert_ne!(v0.token, v1.token);

    // reading doesn't change the version
    let _ = store.read(DocId::Guest(100)).unwrap();
    let v1_again = store.version().unwrap();
    assert_eq!(v1.token, v1_again.token);

    // another write changes it again
    let d = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token);
}

#[test]
fn version_distinguishes_same_length_same_second_writes() {
    // `(mtime, len)` cannot tell two same-length writes within
    // one pmxcfs mtime tick apart, so the token is always computed from the
    // files' actual content.
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    let d = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token, "equal-length same-second writes must differ");
}

#[test]
fn version_reflects_a_change_made_behind_the_stores_back() {
    // No cache means a second `MetaStore` over the same root, or an
    // out-of-band write, is always seen.
    let (dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    std::fs::write(dir.path().join("100.yaml"), "a: 2\n").unwrap();
    assert_ne!(v1.token, store.version().unwrap().token);

    let other = MetaStore::new(dir.path());
    assert_eq!(other.version().unwrap().token, store.version().unwrap().token);
}

#[test]
fn version_token_returns_to_an_earlier_value_when_content_does() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    let d = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let d2 = store.read(DocId::Guest(100)).unwrap().digest;
    store.put_raw(DocId::Guest(100), "a: 1\n", Some(&d2)).unwrap();
    assert_eq!(v1.token, store.version().unwrap().token);
}

#[test]
fn reads_never_lint_but_writes_still_do() {
    // `docs/DESIGN.md` §4. Out-of-band content -- a hand-edited
    // file, a restored backup, pmxcfs replication -- must stay readable, or
    // one bad key in `datacenter.yaml` denies every guest operation
    // cluster-wide and blocks the repair that would fix it.
    let (dir, store) = store();
    let broken = "bad key: 1\nempty:\nlist:\n- ~\n";
    std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();

    let doc = store.read(DocId::Datacenter).unwrap();
    assert_eq!(doc.raw, broken);
    assert_eq!(doc.value["bad key"], json!(1));
    assert_eq!(doc.value["empty"], Value::Null);

    // The repair goes through, even though the *old* content would never
    // pass lint (it used to be re-parsed strictly, just to compute a diff).
    let good = "ok: 1\n";
    let result = store.put_raw(DocId::Datacenter, good, Some(&doc.digest)).unwrap();
    assert_eq!(result.document.raw, good);
    assert_eq!(store.read(DocId::Datacenter).unwrap().value, json!({"ok": 1}));

    // ... and the write-time gate is untouched.
    assert!(matches!(
        store.put_raw(DocId::Datacenter, "bad key: 1\n", None),
        Err(Error::Lint(_))
    ));
    assert_eq!(store.read(DocId::Datacenter).unwrap().value, json!({"ok": 1}));

    // A syntax error is reported *per document*, not raised: see
    // `a_syntax_error_is_reported_per_document_and_never_blocks_a_repair`.
    std::fs::write(dir.path().join("100.yaml"), "a: [\n").unwrap();
    let unparseable = store.read(DocId::Guest(100)).unwrap();
    assert!(unparseable.parse_error.is_some());
    assert_eq!(unparseable.value, json!({}));
}

#[test]
fn a_syntax_error_is_reported_per_document_and_never_blocks_a_repair() {
    // `docs/DESIGN.md` §4: a parse failure is a *per-document* condition.
    // Both write handlers read the document before planning, so a fatal parse
    // here would make a tab or an indentation slip in a hand-edited file
    // unrepairable through the API. Each of these is a real YAML syntax
    // failure, not a lint finding.
    for broken in [
        "a: 1\n\tb: 2\n",            // a tab
        "a: &anc 1\nb: *anc\n",      // an anchor and an alias
        "a: 1\n  b: 2\n",            // an indentation slip
        "a: !!str 1\n",              // an explicit tag
        "a: [\n",                    // an unterminated flow sequence
        "? [1, 2]\n: v\n",           // a complex key
    ] {
        let (dir, store) = store();
        std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();

        let doc = store.read(DocId::Datacenter).unwrap();
        assert!(doc.parse_error.is_some(), "{broken:?} parsed after all");
        // The empty document, the real bytes, the real digest.
        assert_eq!(doc.value, json!({}));
        assert_eq!(doc.raw, broken);
        assert_eq!(doc.digest, digest(broken.as_bytes()));

        // The repair goes through, with the compare-and-swap precondition
        // intact -- `put_raw`'s parse of the *old* bytes used to be the last
        // thing standing between the file and its own fix.
        let good = "ok: 1\n";
        store
            .put_raw(DocId::Datacenter, good, Some(&doc.digest))
            .unwrap_or_else(|e| panic!("{broken:?}: repair refused: {e}"));
        assert_eq!(store.read(DocId::Datacenter).unwrap().value, json!({"ok": 1}));

        // A stale digest is still a 409-shaped refusal, not a free pass.
        std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();
        assert!(matches!(
            store.put_raw(DocId::Datacenter, good, Some("deadbeef")),
            Err(Error::DigestMismatch { .. })
        ));
    }
}

#[test]
fn a_document_larger_than_the_read_cap_is_refused_rather_than_hashed() {
    // `MAX_BYTES` only ever applied to
    // writes, so a multi-megabyte file dropped in out of band was read and
    // SHA-256'd on every request that touched the document -- `datacenter.yaml`
    // on every guest operation, and every 5 s `version()` poll.
    let (dir, store) = store();
    let big = format!("a: \"{}\"\n", "x".repeat(MAX_READ_BYTES as usize));
    std::fs::write(dir.path().join("100.yaml"), &big).unwrap();

    match store.read(DocId::Guest(100)).unwrap_err() {
        Error::TooLarge { size, max } => {
            assert_eq!(max, MAX_READ_BYTES);
            assert!(size > MAX_READ_BYTES);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }

    // A document that was legally written is always readable back: the read
    // cap is eight times the write cap on purpose.
    let legal = format!("a: \"{}\"\n", "x".repeat(400 * 1024));
    store.put_raw(DocId::Guest(101), &legal, None).unwrap();
    assert!(store.read(DocId::Guest(101)).is_ok());

    // ... and the oversized one can still be replaced with something sane
    // (the repair path does not depend on reading the old content).
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    assert_eq!(store.read(DocId::Guest(100)).unwrap().value, json!({"a": 1}));
}

#[test]
fn there_is_one_write_gate_and_it_is_the_document_lint() {
    // `docs/DESIGN.md` §10 deletes `WriteGate` and the privilege-narrowed
    // lint variants: nothing this store writes can fail `model::lint`.
    let (dir, store) = store();
    std::fs::write(dir.path().join("100.yaml"), "bad key: 1\ntraefik:\n  host: a\n").unwrap();

    for (text, expect_lint) in [
        ("bad key: 1\ntraefik:\n  host: b\n", true),
        ("- 1\n- 2\n", true),
        ("traefik:\n  host: b\n", false),
    ] {
        let result = store.put_raw(DocId::Guest(100), text, None);
        assert_eq!(matches!(result, Err(Error::Lint(_))), expect_lint, "{text:?}");
    }
    assert!(matches!(
        store.put_raw(DocId::Guest(100), "a: [\n", None),
        Err(Error::Parse { .. })
    ));
}

#[test]
fn stored_vmids_covers_documents_and_snapshot_copies() {
    // The store's half of the GC (`docs/DESIGN.md` §6): Perl passes the
    // vmlist, and everything here that is not in it is purged. A vmid whose
    // *only* file is a snapshot copy has to be found too.
    let (dir, store) = store();
    assert!(store.stored_vmids().unwrap().is_empty());

    store.put_raw(DocId::Guest(999500), "a: 1\n", None).unwrap();
    store.put_raw(DocId::Guest(100), "a: 1\n", None).unwrap();
    store.put_raw(DocId::Datacenter, "a: 1\n", None).unwrap();
    store.snapshot(100, "before").unwrap();
    // A guest whose document was deleted but whose snapshot copy survives.
    store.put_raw(DocId::Guest(777), "a: 1\n", None).unwrap();
    store.snapshot(777, "only").unwrap();
    store.delete(DocId::Guest(777)).unwrap();
    std::fs::write(dir.path().join(".100.yaml.tmp.node1.42.0"), "junk").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "junk").unwrap();

    assert_eq!(store.stored_vmids().unwrap(), vec![100, 777, 999500]);
}

#[test]
fn too_large_document_is_rejected() {
    let (_dir, store) = store();
    let big = "x".repeat(600 * 1024);
    let text = format!("a: \"{big}\"\n");
    let err = store.put_raw(DocId::Guest(100), &text, None).unwrap_err();
    assert!(matches!(err, Error::TooLarge { .. }));
}
