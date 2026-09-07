//! Integration tests for `pve_meta_core::store::MetaStore`.

use std::thread;
use std::time::Duration;

use pretty_assertions::assert_eq;
use pve_meta_core::error::Error;
use pve_meta_core::format::Format;
use pve_meta_core::store::{DocId, MetaStore, RollbackOutcome};
use serde_json::json;
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
fn patch_creates_document_when_missing() {
    let (_dir, store) = store();
    let doc = store
        .patch(DocId::Guest(100), &json!({"name": "web01"}), None)
        .unwrap();
    assert_eq!(doc.format, Format::Yaml);
    assert_eq!(doc.value, json!({"name": "web01"}));
    assert!(doc.path.exists());

    let reread = store.read(DocId::Guest(100)).unwrap();
    assert_eq!(reread.value, doc.value);
    assert_eq!(reread.digest, doc.digest);
}

#[test]
fn patch_create_with_top_level_delete_fails() {
    let (_dir, store) = store();
    let err = store
        .patch(DocId::Guest(100), &json!({"name": null}), None)
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(DocId::Guest(100))));
}

#[test]
fn patch_invalid_patch_is_rejected() {
    let (_dir, store) = store();
    let err = store
        .patch(DocId::Guest(100), &json!({"bad key": 1}), None)
        .unwrap_err();
    assert!(matches!(err, Error::Lint(_)));
}

#[test]
fn patch_existing_document_merges() {
    let (_dir, store) = store();
    store
        .patch(DocId::Guest(100), &json!({"a": 1, "b": 2}), None)
        .unwrap();
    let doc = store
        .patch(DocId::Guest(100), &json!({"b": null, "c": 3}), None)
        .unwrap();
    assert_eq!(doc.value, json!({"a": 1, "c": 3}));
}

#[test]
fn patch_digest_mismatch_is_rejected() {
    let (_dir, store) = store();
    let doc = store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    let err = store
        .patch(DocId::Guest(100), &json!({"a": 2}), Some("deadbeef"))
        .unwrap_err();
    assert!(matches!(err, Error::DigestMismatch { .. }));
    // correct digest succeeds
    let doc2 = store
        .patch(DocId::Guest(100), &json!({"a": 2}), Some(&doc.digest))
        .unwrap();
    assert_eq!(doc2.value, json!({"a": 2}));
}

#[test]
fn put_raw_creates_and_replaces_and_diffs() {
    let (_dir, store) = store();
    let result = store
        .put_raw(DocId::Guest(100), "a: 1\nb: 2\n", None, None)
        .unwrap();
    assert_eq!(result.document.format, Format::Yaml);
    assert_eq!(result.document.value, json!({"a": 1, "b": 2}));
    // diff against an empty starting document
    assert_eq!(result.touched.len(), 2);

    let result2 = store
        .put_raw(DocId::Guest(100), "a: 10\nc: 3\n", None, Some(&result.document.digest))
        .unwrap();
    assert_eq!(result2.document.value, json!({"a": 10, "c": 3}));
    let mut touched: Vec<String> = result2.touched.iter().map(|t| t.path.to_string()).collect();
    touched.sort();
    assert_eq!(touched, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
}

#[test]
fn put_raw_can_switch_format_and_removes_old_file() {
    let (_dir, store) = store();
    let r1 = store.put_raw(DocId::Guest(100), "a: 1\n", None, None).unwrap();
    let yaml_path = r1.document.path.clone();
    assert!(yaml_path.exists());

    let r2 = store
        .put_raw(DocId::Guest(100), "a = 2\n", Some(Format::Toml), Some(&r1.document.digest))
        .unwrap();
    assert_eq!(r2.document.format, Format::Toml);
    assert!(!yaml_path.exists());
    assert!(r2.document.path.exists());
    assert!(store.locate(DocId::Guest(100)).unwrap().is_some());
}

#[test]
fn put_raw_digest_mismatch() {
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None, None).unwrap();
    let err = store
        .put_raw(DocId::Guest(100), "a: 2\n", None, Some("nope"))
        .unwrap_err();
    assert!(matches!(err, Error::DigestMismatch { .. }));
}

#[test]
fn convert_switches_format_and_preserves_value() {
    let (_dir, store) = store();
    let doc = store
        .patch(DocId::Guest(100), &json!({"a": 1, "b": {"c": 2}}), None)
        .unwrap();
    assert_eq!(doc.format, Format::Yaml);
    let converted = store.convert(DocId::Guest(100), Format::Toml, Some(&doc.digest)).unwrap();
    assert_eq!(converted.format, Format::Toml);
    assert_eq!(converted.value, doc.value);
    assert!(!doc.path.exists());
    assert!(converted.path.exists());

    let reread = store.read(DocId::Guest(100)).unwrap();
    assert_eq!(reread.format, Format::Toml);
    assert_eq!(reread.value, doc.value);
}

#[test]
fn convert_missing_is_not_found() {
    let (_dir, store) = store();
    let err = store.convert(DocId::Guest(100), Format::Toml, None).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)));
}

#[test]
fn delete_removes_document() {
    let (_dir, store) = store();
    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    store.delete(DocId::Guest(100)).unwrap();
    assert!(matches!(store.read(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn delete_missing_is_not_found() {
    let (_dir, store) = store();
    assert!(matches!(store.delete(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn delete_removes_snapshots_too() {
    let (dir, store) = store();
    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    store.snapshot(100, "snap1").unwrap();
    store.delete(DocId::Guest(100)).unwrap();
    assert!(store.list_snapshots(100).unwrap().is_empty());
    assert!(!dir.path().join("100.snap1.yaml").exists());
}

#[test]
fn conflict_when_two_format_files_exist() {
    let (dir, store) = store();
    std::fs::write(dir.path().join("100.yaml"), "a: 1\n").unwrap();
    std::fs::write(dir.path().join("100.toml"), "a = 1\n").unwrap();
    let err = store.read(DocId::Guest(100)).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
    let err2 = store.locate(DocId::Guest(100)).unwrap_err();
    assert!(matches!(err2, Error::Conflict(_)));
}

#[test]
fn list_guests_ignores_snapshots_datacenter_and_tmp_files() {
    let (dir, store) = store();
    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    store.patch(DocId::Guest(200), &json!({"a": 2}), None).unwrap();
    store.patch(DocId::Datacenter, &json!({"settings": {}}), None).unwrap();
    store.snapshot(100, "before").unwrap();
    std::fs::write(dir.path().join(".100.tmp.12345"), "junk").unwrap();

    let guests = store.list_guests().unwrap();
    let vmids: Vec<u32> = guests.iter().map(|g| g.vmid).collect();
    assert_eq!(vmids, vec![100, 200]);
    for g in &guests {
        assert_eq!(g.format, Format::Yaml);
        assert!(g.size > 0);
    }
}

#[test]
fn list_guests_empty_store() {
    let (_dir, store) = store();
    assert!(store.list_guests().unwrap().is_empty());
}

#[test]
fn snapshot_rollback_delete_and_list() {
    let (_dir, store) = store();
    // snapshot with no document is a no-op
    assert!(!store.snapshot(100, "none").unwrap());

    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    assert!(store.snapshot(100, "v1").unwrap());
    assert_eq!(store.list_snapshots(100).unwrap(), vec!["v1".to_string()]);

    store.patch(DocId::Guest(100), &json!({"a": 2}), None).unwrap();
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
fn rollback_without_snapshot_but_with_live_doc_removes_it() {
    let (_dir, store) = store();
    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
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
fn snapshot_of_different_format_replaces_stale_extension() {
    let (dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None, None).unwrap();
    store.snapshot(100, "v1").unwrap();
    assert!(dir.path().join("100.v1.yaml").exists());

    store.convert(DocId::Guest(100), Format::Toml, None).unwrap();
    store.snapshot(100, "v1").unwrap();
    assert!(dir.path().join("100.v1.toml").exists());
    assert!(!dir.path().join("100.v1.yaml").exists());
}

#[test]
fn clone_copies_document_only() {
    let (_dir, store) = store();
    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    store.snapshot(100, "v1").unwrap();

    let cloned = store.clone(100, 200).unwrap();
    assert_eq!(cloned.value, json!({"a": 1}));
    assert!(store.list_snapshots(200).unwrap().is_empty());
}

#[test]
fn clone_fails_if_source_missing_or_target_exists() {
    let (_dir, store) = store();
    assert!(matches!(store.clone(100, 200).unwrap_err(), Error::NotFound(_)));

    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    store.patch(DocId::Guest(200), &json!({"b": 2}), None).unwrap();
    assert!(matches!(store.clone(100, 200).unwrap_err(), Error::Conflict(_)));
}

#[test]
fn destroy_is_delete() {
    let (_dir, store) = store();
    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    store.destroy(100).unwrap();
    assert!(matches!(store.read(DocId::Guest(100)).unwrap_err(), Error::NotFound(_)));
}

#[test]
fn default_format_falls_back_to_yaml_without_datacenter_doc() {
    let (_dir, store) = store();
    assert_eq!(store.default_format().unwrap(), Format::Yaml);
}

#[test]
fn default_format_reads_from_datacenter_settings() {
    let (_dir, store) = store();
    store
        .patch(DocId::Datacenter, &json!({"settings": {"default_format": "toml"}}), None)
        .unwrap();
    assert_eq!(store.default_format().unwrap(), Format::Toml);

    let doc = store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    assert_eq!(doc.format, Format::Toml);
}

#[test]
fn version_token_changes_on_write_not_on_read() {
    let (_dir, store) = store();
    let v0 = store.version().unwrap();

    store.patch(DocId::Guest(100), &json!({"a": 1}), None).unwrap();
    let v1 = store.version().unwrap();
    assert_ne!(v0.token, v1.token);

    // reading doesn't change the version
    let _ = store.read(DocId::Guest(100)).unwrap();
    let v1_again = store.version().unwrap();
    assert_eq!(v1.token, v1_again.token);

    // another write changes it again
    store.patch(DocId::Guest(100), &json!({"a": 2}), None).unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token);
}

#[test]
fn version_distinguishes_same_length_same_second_writes() {
    // Guards against relying on (mtime, len) alone, which has only
    // one-second resolution on pmxcfs: two different byte-for-byte-different
    // writes of equal length, issued back-to-back, must still produce
    // different tokens because the content digest (not just mtime/len) is
    // what feeds the token.
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None, None).unwrap();
    let v1 = store.version().unwrap();
    store
        .put_raw(DocId::Guest(100), "a: 2\n", None, Some(&store.read(DocId::Guest(100)).unwrap().digest))
        .unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1.token, v2.token, "equal-length same-second writes must differ");
}

#[test]
fn version_cache_still_correct_after_it_goes_stale() {
    // Exercises the >2s-old cache-hit path: after the cache entry is old
    // enough to be trusted, the token must still reflect the true content
    // (unchanged here), and a subsequent real change must still be seen.
    let (_dir, store) = store();
    store.put_raw(DocId::Guest(100), "a: 1\n", None, None).unwrap();
    let v1 = store.version().unwrap();
    thread::sleep(Duration::from_millis(2100));
    let v1_stale = store.version().unwrap();
    assert_eq!(v1.token, v1_stale.token, "unchanged content must keep the same token");

    store
        .put_raw(DocId::Guest(100), "a: 2\n", None, Some(&store.read(DocId::Guest(100)).unwrap().digest))
        .unwrap();
    let v2 = store.version().unwrap();
    assert_ne!(v1_stale.token, v2.token, "a real change must still be detected");
}

#[test]
fn too_large_document_is_rejected() {
    let (_dir, store) = store();
    let big = "x".repeat(600 * 1024);
    let text = format!("a: \"{big}\"\n");
    let err = store.put_raw(DocId::Guest(100), &text, None, None).unwrap_err();
    assert!(matches!(err, Error::TooLarge { .. }));
}
