//! Integration tests for `pve_meta_core::store::MetaStore`.

use pretty_assertions::assert_eq;
use pve_meta_core::digest::digest;
use pve_meta_core::error::Error;
use pve_meta_core::store::{DocId, MetaStore, RegistryKind, RollbackOutcome, MAX_READ_BYTES};
use serde_json::{json, Value};
use tempfile::tempdir;

/// A store rooted in a fresh temp directory, with its own registry
/// directories under it rather than the real `/usr/share/pve-meta` and
/// `/etc/pve/meta.d`, so this suite never touches the live cluster.
fn store() -> (tempfile::TempDir, MetaStore) {
    let dir = tempdir().unwrap();
    let store = MetaStore::with_registry_dirs(
        dir.path(),
        vec![packaged_prefix_dir(dir.path()), cluster_prefix_dir(dir.path())],
    );
    (dir, store)
}

fn packaged_prefix_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("registry/prefixes-packaged")
}

fn cluster_prefix_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("registry/prefixes-cluster")
}

fn prefix(name: &str) -> DocId {
    DocId::Registry(RegistryKind::PrefixDef, name.to_string())
}

#[test]
fn read_missing_is_not_found() {
    let (_dir, store) = store();
    let err = store.read(&DocId::Guest(100)).unwrap_err();
    assert!(matches!(err, Error::NotFound(DocId::Guest(100))));
}

#[test]
fn put_raw_creates_and_replaces() {
    let (_dir, store) = store();
    let result = store.put_raw(&DocId::Guest(100), "a: 1\nb: 2\n", None).unwrap();
    assert_eq!(result.value, json!({"a": 1, "b": 2}));
    assert!(result.path.ends_with("100.yaml"));

    let result2 = store
        .put_raw(&DocId::Guest(100), "a: 10\nc: 3\n", Some(&result.digest))
        .unwrap();
    assert_eq!(result2.value, json!({"a": 10, "c": 3}));
    assert_eq!(store.read(&DocId::Guest(100)).unwrap(), result2);
}

#[test]
fn put_raw_digest_mismatch() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let err = store
        .put_raw(&DocId::Guest(100), "a: 2\n", Some("nope"))
        .unwrap_err();
    assert!(matches!(err, Error::DigestMismatch { .. }));
}

#[test]
fn put_raw_empty_digest_matches_a_missing_document() {
    // `GET` reports digest "" for a non-existent document, so
    // the documented GET-then-PUT-with-digest create flow must work.
    let (_dir, store) = store();
    let result = store.put_raw(&DocId::Guest(100), "a: 1\n", Some("")).unwrap();
    assert_eq!(result.value, json!({"a": 1}));

    // Once it exists, "" no longer matches.
    let err = store
        .put_raw(&DocId::Guest(100), "a: 2\n", Some(""))
        .unwrap_err();
    match err {
        Error::DigestMismatch { expected, actual } => {
            assert_eq!(expected, "");
            assert_eq!(actual, result.digest);
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
}

#[test]
fn put_raw_non_empty_digest_against_a_missing_document_is_a_mismatch() {
    let (_dir, store) = store();
    let err = store
        .put_raw(&DocId::Guest(100), "a: 1\n", Some("deadbeef"))
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
        store.put_raw(&DocId::Guest(100), "a: [\n", None),
        Err(Error::Parse { .. })
    ));
    assert!(matches!(
        store.put_raw(&DocId::Guest(100), "a: ~\n", None),
        Err(Error::Lint(_))
    ));
    assert!(matches!(
        store.put_raw(&DocId::Guest(100), "- 1\n- 2\n", None),
        Err(Error::Lint(_))
    ));
    // Nothing was written.
    assert!(store.digest_of(&DocId::Guest(100)).unwrap().is_none());
}

#[test]
fn put_raw_normalizes_the_trailing_newline_and_digest_matches_the_file() {
    let (_dir, store) = store();
    let r = store.put_raw(&DocId::Guest(100), "a: 1", None).unwrap();
    assert_eq!(r.raw, "a: 1\n");
    let on_disk = std::fs::read(&r.path).unwrap();
    assert_eq!(digest(&on_disk), r.digest);
}

#[test]
fn delete_removes_document() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.delete(&DocId::Guest(100)).unwrap();
    assert!(matches!(store.read(&DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn delete_is_idempotent_and_says_whether_it_removed_anything() {
    // Deleting a document is a request for it to be gone; it being already
    // gone -- because a concurrent DELETE or the GC won the race -- is that
    // request satisfied.
    let (_dir, store) = store();
    assert!(!store.delete(&DocId::Guest(100)).unwrap());
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    assert!(store.delete(&DocId::Guest(100)).unwrap());
    assert!(!store.delete(&DocId::Guest(100)).unwrap());
    assert!(!store.delete_snapshot(100, "nope").unwrap());
}

#[test]
fn a_file_that_vanishes_between_syscalls_is_not_found_not_an_io_error() {
    // Reads run unlocked while writes hold `pve-meta-<id>`, so a file can
    // disappear between any two syscalls of a read. Simulated here by
    // removing it before the read rather than mid-read -- the point is
    // that every path answers NotFound rather than propagating
    // `io::ErrorKind::NotFound` as `Error::Io`.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    std::fs::remove_file(dir.path().join("100.yaml")).unwrap();

    assert!(matches!(store.read(&DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
    assert_eq!(store.digest_of(&DocId::Guest(100)).unwrap(), None);
    assert!(store.check_precondition(&DocId::Guest(100), Some("")).is_ok());
    assert!(!store.snapshot(100, "s").unwrap());
    assert_eq!(store.purge(100).unwrap(), 0);
    assert!(store.version().is_ok());
}

#[test]
fn version_does_not_read_a_file_above_the_read_cap() {
    // The 5 s poll's cost is bounded by the same rule document reads are:
    // one multi-megabyte file dropped in out of band must not be SHA-256'd
    // by every open UI, forever. Its identity becomes a surrogate over
    // (len, mtime) -- which still moves when the file does.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let path = dir.path().join("999500.yaml");
    std::fs::write(&path, format!("a: \"{}\"\n", "x".repeat(MAX_READ_BYTES as usize))).unwrap();

    let v1 = store.version().unwrap();
    assert_eq!(store.version().unwrap().token, v1.token, "stable across polls");

    // `digest_of` answers for it without reading it, and it is the same
    // string the compare-and-swap precondition compares against.
    let dig = store.digest_of(&DocId::Guest(999500)).unwrap().unwrap();
    assert_eq!(dig.len(), 64);
    assert!(store.check_precondition(&DocId::Guest(999500), Some(&dig)).is_ok());
    assert!(matches!(
        store.check_precondition(&DocId::Guest(999500), Some("deadbeef")),
        Err(Error::DigestMismatch { .. })
    ));

    // ... and the token still moves when the oversized file changes.
    std::fs::write(&path, format!("a: \"{}\"\n", "y".repeat(MAX_READ_BYTES as usize + 9))).unwrap();
    assert_ne!(store.version().unwrap().token, v1.token);
}

#[test]
fn delete_leaves_snapshots_alone() {
    // `DELETE /meta/guests/{vmid}` has no concept of snapshots, so `delete`
    // must never cascade into them. Only `purge` (the GC) does.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "snapA").unwrap();
    store.delete(&DocId::Guest(100)).unwrap();
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["snapA".to_string()]);
    assert!(dir.path().join("100.snapA.yaml").exists());
}

#[test]
fn purge_removes_the_document_and_every_snapshot() {
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "snapA").unwrap();
    store.snapshot(100, "snapB").unwrap();
    assert_eq!(store.purge(100).unwrap(), 3);
    assert!(matches!(store.read(&DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
    assert!(store.list_snapshots(100).unwrap().is_empty());
    assert!(!dir.path().join("100.snapA.yaml").exists());
    assert!(!dir.path().join("100.snapB.yaml").exists());
}

#[test]
fn purge_is_idempotent_and_cleans_left_behind_snapshots() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "leftover").unwrap();
    store.delete(&DocId::Guest(100)).unwrap();
    // No live document, but a snapshot survives the API delete.
    assert_eq!(store.purge(100).unwrap(), 1);
    assert!(store.list_snapshots(100).unwrap().is_empty());
    assert_eq!(store.purge(100).unwrap(), 0);
}

#[test]
fn api_delete_then_rollback_does_not_lose_snapshot_metadata() {
    // Pin: after a DELETE and a fresh write, a rollback to an earlier
    // snapshot must still restore it, not mistake the DELETE for proof
    // there was no snapshot.
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "traefik:\n  host: a\n", None).unwrap();
    store.snapshot(100, "snapA").unwrap();

    // A user empties the document through the API...
    store.delete(&DocId::Guest(100)).unwrap();
    // ... then writes new metadata ...
    store.put_raw(&DocId::Guest(100), "traefik:\n  host: b\n", None).unwrap();
    // ... then rolls the guest back to snapA.
    assert_eq!(store.rollback(100, "snapA").unwrap(), RollbackOutcome::Restored);
    assert_eq!(
        store.read(&DocId::Guest(100)).unwrap().value,
        json!({"traefik": {"host": "a"}})
    );
}

#[test]
fn snapshot_rollback_delete_and_list() {
    let (_dir, store) = store();
    // snapshot with no document is a no-op
    assert!(!store.snapshot(100, "none").unwrap());

    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    assert!(store.snapshot(100, "v1").unwrap());
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["v1".to_string()]);

    let d = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    assert!(store.snapshot(100, "v2").unwrap());
    assert_eq!(
        store.list_snapshots(100).unwrap(),
        vec!["v1".to_string(), "v2".to_string()]
    );

    // rollback restores content
    let outcome = store.rollback(100, "v1").unwrap();
    assert_eq!(outcome, RollbackOutcome::Restored);
    assert_eq!(store.read(&DocId::Guest(100)).unwrap().value, json!({"a": 1}));

    store.delete_snapshot(100, "v2").unwrap();
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["v1".to_string()]);

    // deleting a missing snapshot is idempotent
    store.delete_snapshot(100, "v2").unwrap();
}

#[test]
fn snapshot_re_snapshot_overwrites() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "v1").unwrap();
    let d = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    store.snapshot(100, "v1").unwrap();
    store.rollback(100, "v1").unwrap();
    assert_eq!(store.read(&DocId::Guest(100)).unwrap().value, json!({"a": 2}));
}

#[test]
fn rollback_without_snapshot_but_with_live_doc_removes_it() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let outcome = store.rollback(100, "never-existed").unwrap();
    assert_eq!(outcome, RollbackOutcome::RemovedNoSnapshot);
    assert!(matches!(store.read(&DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
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
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.put_raw(&DocId::Guest(1000), "a: 1\n", None).unwrap();
    store.put_raw(&DocId::Guest(200), "a: 1\n", None).unwrap();
    store.snapshot(100, "before").unwrap();
    std::fs::write(dir.path().join(".100.yaml.tmp.node1.42.0"), "junk").unwrap();
    std::fs::write(dir.path().join("100.bad name.yaml"), "a: 1\n").unwrap();

    assert_eq!(store.list_snapshots(100).unwrap(), vec!["before".to_string()]);
    assert!(store.list_snapshots(1000).unwrap().is_empty());
}

#[test]
fn write_atomic_leaves_no_temp_files_behind() {
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let d = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
}

#[test]
fn version_moves_for_a_snapshot_and_a_stray_file_too() {
    // The token is one hash over every file in the store root plus the
    // prefix directories: a snapshot copy and a stray file with no vmid in
    // its name (a `datacenter.yaml` left by a release that had such a
    // document, say) both move it, even though neither is addressable
    // through the API.
    let (dir, store) = store();
    let v0 = store.version().unwrap();

    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    assert_ne!(v0.token, v1.token);

    store.snapshot(100, "before").unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token, "a snapshot moves the token");

    std::fs::write(dir.path().join("datacenter.yaml"), "note: x\n").unwrap();
    let v3 = store.version().unwrap();
    assert_ne!(v2.token, v3.token, "a stray file moves the token too");
}

#[test]
fn version_token_changes_on_write_not_on_read() {
    let (_dir, store) = store();
    let v0 = store.version().unwrap();

    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    assert_ne!(v0.token, v1.token);

    // reading doesn't change the version
    let _ = store.read(&DocId::Guest(100)).unwrap();
    let v1_again = store.version().unwrap();
    assert_eq!(v1.token, v1_again.token);

    // another write changes it again
    let d = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token);
}

#[test]
fn version_distinguishes_same_length_same_second_writes() {
    // `(mtime, len)` cannot tell two same-length writes within
    // one pmxcfs mtime tick apart, so the token is always computed from the
    // files' actual content.
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    let d = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token, "equal-length same-second writes must differ");
}

#[test]
fn version_reflects_a_change_made_behind_the_stores_back() {
    // No cache means a second `MetaStore` over the same root, or an
    // out-of-band write, is always seen.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    std::fs::write(dir.path().join("100.yaml"), "a: 2\n").unwrap();
    assert_ne!(v1.token, store.version().unwrap().token);

    let other = MetaStore::new(dir.path());
    assert_eq!(other.version().unwrap().token, store.version().unwrap().token);
}

#[test]
fn version_token_returns_to_an_earlier_value_when_content_does() {
    let (_dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    let v1 = store.version().unwrap();
    let d = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 2\n", Some(&d)).unwrap();
    let d2 = store.read(&DocId::Guest(100)).unwrap().digest;
    store.put_raw(&DocId::Guest(100), "a: 1\n", Some(&d2)).unwrap();
    assert_eq!(v1.token, store.version().unwrap().token);
}

#[test]
fn reads_never_lint_but_writes_still_do() {
    // `docs/DESIGN.md` §5: the lint runs on the planned document, not on a read.
    let (dir, store) = store();
    let broken = "bad key: 1\nempty:\nlist:\n- ~\n";
    std::fs::write(dir.path().join("200.yaml"), broken).unwrap();

    let doc = store.read(&DocId::Guest(200)).unwrap();
    assert_eq!(doc.raw, broken);
    assert_eq!(doc.value["bad key"], json!(1));
    assert_eq!(doc.value["empty"], Value::Null);

    // The repair goes through, even though the *old* content would never
    // pass lint.
    let good = "ok: 1\n";
    let result = store.put_raw(&DocId::Guest(200), good, Some(&doc.digest)).unwrap();
    assert_eq!(result.raw, good);
    assert_eq!(store.read(&DocId::Guest(200)).unwrap().value, json!({"ok": 1}));

    // ... and the write-time gate is untouched.
    assert!(matches!(
        store.put_raw(&DocId::Guest(200), "bad key: 1\n", None),
        Err(Error::Lint(_))
    ));
    assert_eq!(store.read(&DocId::Guest(200)).unwrap().value, json!({"ok": 1}));

    // A syntax error is reported *per document*, not raised: see
    // `a_syntax_error_is_reported_per_document_and_never_blocks_a_repair`.
    std::fs::write(dir.path().join("100.yaml"), "a: [\n").unwrap();
    let unparseable = store.read(&DocId::Guest(100)).unwrap();
    assert!(unparseable.parse_error.is_some());
    assert_eq!(unparseable.value, json!({}));
}

#[test]
fn a_syntax_error_is_reported_per_document_and_never_blocks_a_repair() {
    // `docs/DESIGN.md` §1 invariant 5: a parse failure is per-document, never
    // blocking. Two distinct reasons a document can fail to parse: an
    // ordinary syntax error, and a construct this store's strict reader
    // refuses (`docs/DESIGN.md` §1 invariant 6).
    for broken in [
        "a: [\n",               // an unterminated flow sequence
        "a: &anc 1\nb: *anc\n", // an anchor and an alias
    ] {
        let (dir, store) = store();
        std::fs::write(dir.path().join("200.yaml"), broken).unwrap();

        let doc = store.read(&DocId::Guest(200)).unwrap();
        assert!(doc.parse_error.is_some(), "{broken:?} parsed after all");
        // The empty document, the real bytes, the real digest.
        assert_eq!(doc.value, json!({}));
        assert_eq!(doc.raw, broken);
        assert_eq!(doc.digest, digest(broken.as_bytes()));

        // The repair goes through, with the compare-and-swap precondition
        // intact, even though the *old* bytes never parse.
        let good = "ok: 1\n";
        store
            .put_raw(&DocId::Guest(200), good, Some(&doc.digest))
            .unwrap_or_else(|e| panic!("{broken:?}: repair refused: {e}"));
        assert_eq!(store.read(&DocId::Guest(200)).unwrap().value, json!({"ok": 1}));

        // A stale digest is still a 409-shaped refusal, not a free pass.
        std::fs::write(dir.path().join("200.yaml"), broken).unwrap();
        assert!(matches!(
            store.put_raw(&DocId::Guest(200), good, Some("deadbeef")),
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

    match store.read(&DocId::Guest(100)).unwrap_err() {
        Error::TooLarge { size, max } => {
            assert_eq!(max, MAX_READ_BYTES);
            assert!(size > MAX_READ_BYTES);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }

    // A document that was legally written is always readable back: the read
    // cap is eight times the write cap on purpose.
    let legal = format!("a: \"{}\"\n", "x".repeat(400 * 1024));
    store.put_raw(&DocId::Guest(101), &legal, None).unwrap();
    assert!(store.read(&DocId::Guest(101)).is_ok());

    // ... and the oversized one can still be replaced with something sane
    // (the repair path does not depend on reading the old content).
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    assert_eq!(store.read(&DocId::Guest(100)).unwrap().value, json!({"a": 1}));
}

#[test]
fn there_is_one_write_gate_and_it_is_the_document_lint() {
    // The document lint (`model::lint`) is the only write gate: nothing
    // this store writes can bypass it.
    let (dir, store) = store();
    std::fs::write(dir.path().join("100.yaml"), "bad key: 1\ntraefik:\n  host: a\n").unwrap();

    for (text, expect_lint) in [
        ("bad key: 1\ntraefik:\n  host: b\n", true),
        ("- 1\n- 2\n", true),
        ("traefik:\n  host: b\n", false),
    ] {
        let result = store.put_raw(&DocId::Guest(100), text, None);
        assert_eq!(matches!(result, Err(Error::Lint(_))), expect_lint, "{text:?}");
    }
    assert!(matches!(
        store.put_raw(&DocId::Guest(100), "a: [\n", None),
        Err(Error::Parse { .. })
    ));
}

#[test]
fn stored_vmids_covers_documents_and_snapshot_copies() {
    // The store's half of the orphan GC (`docs/DESIGN.md` §7): a vmid whose
    // *only* file is a snapshot copy has to be found too.
    let (dir, store) = store();
    assert!(store.stored_vmids().unwrap().is_empty());

    store.put_raw(&DocId::Guest(999500), "a: 1\n", None).unwrap();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    // A stray file with no vmid in its name is not a guest.
    std::fs::write(dir.path().join("datacenter.yaml"), "a: 1\n").unwrap();
    store.snapshot(100, "before").unwrap();
    // A guest whose document was deleted but whose snapshot copy survives.
    store.put_raw(&DocId::Guest(777), "a: 1\n", None).unwrap();
    store.snapshot(777, "only").unwrap();
    store.delete(&DocId::Guest(777)).unwrap();
    std::fs::write(dir.path().join(".100.yaml.tmp.node1.42.0"), "junk").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "junk").unwrap();

    assert_eq!(store.stored_vmids().unwrap(), vec![100, 777, 999500]);
}

#[test]
fn a_file_the_store_cannot_name_is_listed_rather_than_skipped() {
    // A typo'd or leftover file name is otherwise invisible: `stored_vmids`
    // passes it over while `version` hashes it, so `pve-meta ls` shows it.
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "before").unwrap();
    for stray in ["datacenter.yaml", "100.bad name.yaml", "notes.txt", "100"] {
        std::fs::write(dir.path().join(stray), "a: 1\n").unwrap();
    }
    // Our own write-in-progress temp file is not a stray.
    std::fs::write(dir.path().join(".100.yaml.tmp.node1.42.0"), "junk").unwrap();

    assert_eq!(store.stored_vmids().unwrap(), vec![100]);
    assert_eq!(
        store.unknown_files().unwrap(),
        vec!["100", "100.bad name.yaml", "datacenter.yaml", "notes.txt"],
    );
}

#[test]
fn too_large_document_is_rejected() {
    let (_dir, store) = store();
    let big = "x".repeat(600 * 1024);
    let text = format!("a: \"{big}\"\n");
    let err = store.put_raw(&DocId::Guest(100), &text, None).unwrap_err();
    assert!(matches!(err, Error::TooLarge { .. }));
}

// --- registry documents (DocId::Registry) -----------------------------------

#[test]
fn a_registry_document_is_written_to_its_own_directory_not_the_root() {
    let (dir, store) = store();
    store
        .put_raw(&prefix("homelab"), "description: Home\n", None)
        .unwrap();

    assert!(cluster_prefix_dir(dir.path()).join("homelab.yaml").is_file());
    assert!(!dir.path().join("homelab.yaml").exists());
    assert_eq!(store.read(&prefix("homelab")).unwrap().raw, "description: Home\n");
}

#[test]
fn a_registry_write_creates_the_override_and_leaves_the_packaged_file_alone() {
    let (dir, store) = store();
    let packaged = packaged_prefix_dir(dir.path());
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::write(packaged.join("traefik.yaml"), "description: packaged\n").unwrap();

    // A read sees the packaged file, and its digest is what a write must carry.
    let read = store.read(&prefix("traefik")).unwrap();
    assert_eq!(read.raw, "description: packaged\n");
    assert_eq!(read.path, packaged.join("traefik.yaml"));

    // The compare-and-swap is against what was read, so overriding a packaged
    // file is an ordinary write and not a spurious conflict.
    store
        .put_raw(&prefix("traefik"), "description: cluster\n", Some(&read.digest))
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(packaged.join("traefik.yaml")).unwrap(),
        "description: packaged\n",
        "the packaged file belongs to its .deb and must not be touched",
    );
    let after = store.read(&prefix("traefik")).unwrap();
    assert_eq!(after.raw, "description: cluster\n");
    assert_eq!(after.path, cluster_prefix_dir(dir.path()).join("traefik.yaml"));
}

#[test]
fn deleting_a_registry_override_falls_back_to_the_packaged_file() {
    let (dir, store) = store();
    let packaged = packaged_prefix_dir(dir.path());
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::write(packaged.join("traefik.yaml"), "description: packaged\n").unwrap();
    store
        .put_raw(&prefix("traefik"), "description: cluster\n", None)
        .unwrap();

    assert!(store.delete(&prefix("traefik")).unwrap());
    assert_eq!(
        store.read(&prefix("traefik")).unwrap().raw,
        "description: packaged\n",
        "deleting the override reverts to the packaged prefix",
    );

    // And there is nothing left of ours to delete: the packaged file stays.
    assert!(!store.delete(&prefix("traefik")).unwrap());
    assert!(packaged.join("traefik.yaml").is_file());
}

/// The loader and the store agree on which file a name means -- also when
/// the override is broken: the read opens the override for repair, and the
/// loader lists that same file as the failure, not the packaged file as a
/// prefix.
#[test]
fn a_malformed_override_is_the_file_a_read_opens_and_the_loader_reports() {
    let (dir, store) = store();
    let packaged = packaged_prefix_dir(dir.path());
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::write(packaged.join("traefik.yaml"), "selector: {all: true}\n").unwrap();
    store
        .put_raw(&prefix("traefik"), "selector: {all: true}\n", None)
        .unwrap();
    let cluster_file = cluster_prefix_dir(dir.path()).join("traefik.yaml");
    std::fs::write(&cluster_file, "selector: {nonsense: true}\n").unwrap();

    let read = store.read(&prefix("traefik")).unwrap();
    assert_eq!(read.path, cluster_file, "the read opens the override");
    assert_eq!(read.raw, "selector: {nonsense: true}\n");

    let (loaded, failures) = store.registry().unwrap().list_prefixes().unwrap();
    assert!(loaded.is_empty(), "the packaged file is shadowed, not re-activated");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].name, "traefik");

    // Removing the override is the repair: the packaged file is in effect again.
    assert!(store.delete(&prefix("traefik")).unwrap());
    let (loaded, failures) = store.registry().unwrap().list_prefixes().unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(failures.is_empty());
}

#[test]
fn a_missing_registry_document_is_not_found() {
    let (_dir, store) = store();
    let err = store.read(&prefix("nope")).unwrap_err();
    assert!(matches!(err, Error::NotFound(DocId::Registry(RegistryKind::PrefixDef, ref n)) if n == "nope"));
    assert_eq!(store.digest_of(&prefix("nope")).unwrap(), None);
}

#[test]
fn version_notices_a_shadowed_registry_document_changing_too() {
    let (dir, store) = store();
    let packaged = packaged_prefix_dir(dir.path());
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::write(packaged.join("traefik.yaml"), "description: packaged\n").unwrap();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store
        .put_raw(&prefix("traefik"), "description: cluster\n", None)
        .unwrap();

    // The shadowed file still moves the token: it is part of what the loaders
    // see, and a poll that misses a change is worse than one that reloads.
    let before = store.version().unwrap().token;
    std::fs::write(packaged.join("traefik.yaml"), "description: edited\n").unwrap();
    assert_ne!(store.version().unwrap().token, before);
}

#[test]
fn a_registry_write_leaves_no_temp_files_behind_in_its_own_directory() {
    let (dir, store) = store();
    store.put_raw(&prefix("homelab"), "description: a\n", None).unwrap();
    let d = store.read(&prefix("homelab")).unwrap().digest;
    store
        .put_raw(&prefix("homelab"), "description: b\n", Some(&d))
        .unwrap();

    let leftovers: Vec<String> = std::fs::read_dir(cluster_prefix_dir(dir.path()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
}

#[test]
fn a_nested_prefix_is_a_dotted_file_name_and_still_moves_the_token() {
    let (dir, store) = store();
    // `homelab.docker.yaml` declares the prefix `homelab.docker` -- the file
    // name *is* the prefix, so dots in it are ordinary.
    let nested = DocId::Registry(RegistryKind::PrefixDef, "homelab.docker".to_string());
    let before = store.version().unwrap().token;
    let written = store.put_raw(&nested, "selector: {all: true}\n", None).unwrap();
    assert_eq!(
        written.path,
        cluster_prefix_dir(dir.path()).join("homelab.docker.yaml"),
    );
    assert_ne!(store.version().unwrap().token, before);
}

// --- a store that is not there (docs/DESIGN.md §1 invariant 3) --------------

#[test]
fn a_store_whose_cluster_marker_is_missing_refuses_every_operation() {
    let (dir, store) = store();
    store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
    store.snapshot(100, "before").unwrap();
    let marker = dir.path().join("local");
    let store = store.with_cluster_marker(&marker);

    // pmxcfs not mounted: every public method refuses the same way, rather
    // than reading the directory that is right there as an empty store.
    type Call = Box<dyn Fn(&MetaStore) -> Result<(), Error>>;
    let calls: Vec<(&str, Call)> = vec![
        ("check_available", Box::new(|s| s.check_available())),
        ("read", Box::new(|s| s.read(&DocId::Guest(100)).map(drop))),
        ("read (registry)", Box::new(|s| s.read(&prefix("traefik")).map(drop))),
        ("digest_of", Box::new(|s| s.digest_of(&DocId::Guest(100)).map(drop))),
        ("check_precondition", Box::new(|s| s.check_precondition(&DocId::Guest(100), Some("")))),
        ("put_raw", Box::new(|s| s.put_raw(&DocId::Guest(101), "b: 1\n", None).map(drop))),
        ("delete", Box::new(|s| s.delete(&DocId::Guest(100)).map(drop))),
        ("stored_vmids", Box::new(|s| s.stored_vmids().map(drop))),
        ("unknown_files", Box::new(|s| s.unknown_files().map(drop))),
        ("list_snapshots", Box::new(|s| s.list_snapshots(100).map(drop))),
        ("snapshot", Box::new(|s| s.snapshot(100, "again").map(drop))),
        ("rollback", Box::new(|s| s.rollback(100, "before").map(drop))),
        ("delete_snapshot", Box::new(|s| s.delete_snapshot(100, "before").map(drop))),
        ("purge", Box::new(|s| s.purge(100).map(drop))),
        ("version", Box::new(|s| s.version().map(drop))),
        ("registry", Box::new(|s| s.registry().map(drop))),
    ];
    for (name, call) in &calls {
        assert!(matches!(call(&store), Err(Error::Unavailable(_))), "{name}");
    }
    // A marker that is there but is not what pmxcfs provides is no better.
    std::fs::create_dir(&marker).unwrap();
    assert!(matches!(store.check_available(), Err(Error::Unavailable(_))));
    assert!(!dir.path().join("101.yaml").exists(), "nothing was written");

    // Mounted: the same store answers.
    std::fs::remove_dir(&marker).unwrap();
    std::os::unix::fs::symlink(dir.path(), &marker).unwrap();
    assert_eq!(store.read(&DocId::Guest(100)).unwrap().raw, "a: 1\n");
    assert_eq!(store.stored_vmids().unwrap(), vec![100]);
}

#[test]
fn a_directory_that_cannot_be_read_is_an_error_not_an_empty_one() {
    // Not there is empty. Anything else -- here a file where a directory should
    // be, which fails like an unreadable directory -- is the error it is.
    let dir = tempdir().unwrap();
    let not_a_dir = dir.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let store = MetaStore::with_registry_dirs(&not_a_dir, vec![]);
    assert!(matches!(store.stored_vmids(), Err(Error::Io(_))));
    assert!(matches!(store.list_snapshots(100), Err(Error::Io(_))));
    assert!(matches!(store.version(), Err(Error::Io(_))));

    let store = MetaStore::with_registry_dirs(dir.path(), vec![not_a_dir.clone()]);
    let registry = store.registry().unwrap();
    assert!(matches!(registry.list_prefixes(), Err(Error::Io(_))));
    assert!(matches!(store.version(), Err(Error::Io(_))));

    // And a missing one is still nothing at all.
    let gone = dir.path().join("gone");
    let store = MetaStore::with_registry_dirs(&gone, vec![gone.clone()]);
    assert_eq!(store.stored_vmids().unwrap(), Vec::<u32>::new());
    assert!(store.version().is_ok());
}

#[test]
fn one_entry_that_cannot_be_looked_at_costs_that_entry_not_the_answer() {
    // A symlink loop fails `stat` with ELOOP: there, and unreadable.
    let (dir, store) = store();
    let prefixes = cluster_prefix_dir(dir.path());
    std::fs::create_dir_all(&prefixes).unwrap();
    std::fs::write(prefixes.join("good.yaml"), "selector: {all: true}\n").unwrap();
    std::os::unix::fs::symlink("loop.yaml", prefixes.join("loop.yaml")).unwrap();
    let (loaded, failures) = store.registry().unwrap().list_prefixes().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(failures.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["loop"]);
}
