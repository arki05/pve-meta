//! Integration tests for the TOML format-preserving edit engine
//! (`pve_meta_core::edit::apply_patch_text` with `Format::Toml`).

use pretty_assertions::assert_eq;
use pve_meta_core::edit::apply_patch_text;
use pve_meta_core::format::{parse, Format};
use serde_json::json;

fn edit(text: &str, patch: serde_json::Value) -> pve_meta_core::edit::EditResult {
    apply_patch_text(Format::Toml, text, &patch).unwrap_or_else(|e| panic!("edit failed: {e}"))
}

#[test]
fn comments_and_blank_lines_around_untouched_keys_survive() {
    let text = "\
# top of file comment

[section]
# comment above a
a = 1 # trailing on a

# comment above b
b = 2
";
    let res = edit(text, json!({"section": {"a": 100}}));
    let expected = "\
# top of file comment

[section]
# comment above a
a = 100 # trailing on a

# comment above b
b = 2
";
    assert_eq!(res.text, expected);
    assert_eq!(res.value, json!({"section": {"a": 100, "b": 2}}));
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
}

#[test]
fn header_comment_survives_when_editing_sibling_table() {
    let text = "\
# datacenter meta
[settings]
default_format = \"yaml\"

[other]
x = 1
";
    let res = edit(text, json!({"other": {"x": 2}}));
    assert!(res.text.starts_with("# datacenter meta\n"));
    assert!(res.text.contains("default_format = \"yaml\""));
    assert!(res.text.contains("x = 2"));
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
}

#[test]
fn changing_one_scalar_changes_exactly_one_line() {
    let text = "a = 1\nb = 2\nc = 3\n";
    let res = edit(text, json!({"b": 20}));
    let before: Vec<&str> = text.lines().collect();
    let after: Vec<&str> = res.text.lines().collect();
    assert_eq!(before.len(), after.len());
    let diff_lines: Vec<(usize, (&str, &str))> = before
        .iter()
        .zip(after.iter())
        .enumerate()
        .filter(|(_, (b, a))| b != a)
        .map(|(i, (b, a))| (i, (*b, *a)))
        .collect();
    assert_eq!(diff_lines, vec![(1, ("b = 2", "b = 20"))]);
}

#[test]
fn appending_a_new_scalar_key() {
    let text = "a = 1\n";
    let res = edit(text, json!({"b": "new"}));
    assert_eq!(res.text, "a = 1\nb = \"new\"\n");
    assert_eq!(res.value, json!({"a": 1, "b": "new"}));
}

#[test]
fn appending_a_new_key_inside_existing_table() {
    let text = "[a]\nx = 1\n";
    let res = edit(text, json!({"a": {"y": 2}}));
    assert_eq!(res.text, "[a]\nx = 1\ny = 2\n");
}

#[test]
fn creating_missing_nested_tables() {
    let text = "top = 1\n";
    let res = edit(text, json!({"a": {"b": {"c": 1}}}));
    assert_eq!(res.text, "top = 1\n\n[a]\n\n[a.b]\nc = 1\n");
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
    assert_eq!(res.value, json!({"top": 1, "a": {"b": {"c": 1}}}));
}

#[test]
fn deleting_a_scalar_key_leaves_table_when_not_empty() {
    let text = "[a]\nx = 1\ny = 2\n";
    let res = edit(text, json!({"a": {"x": null}}));
    assert_eq!(res.text, "[a]\ny = 2\n");
    assert_eq!(res.value, json!({"a": {"y": 2}}));
}

#[test]
fn deleting_last_key_removes_table_header() {
    let text = "top = 1\n\n[a]\nx = 1\n";
    let res = edit(text, json!({"a": {"x": null}}));
    assert_eq!(res.text, "top = 1\n");
    assert_eq!(res.value, json!({"top": 1}));
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
}

#[test]
fn deleting_whole_table_via_top_level_null() {
    let text = "[a]\nx = 1\ny = 2\n\n[b]\nz = 3\n";
    let res = edit(text, json!({"a": null}));
    assert_eq!(res.text, "\n[b]\nz = 3\n");
    assert_eq!(res.value, json!({"b": {"z": 3}}));
}

#[test]
fn replacing_array_of_scalars_keeps_it_inline() {
    let text = "tags = [\"a\", \"b\"]\n";
    let res = edit(text, json!({"tags": ["c", "d", "e"]}));
    assert_eq!(res.text, "tags = [\"c\", \"d\", \"e\"]\n");
}

#[test]
fn array_of_tables_replace_stays_array_of_tables() {
    let text = "[[items]]\nn = 1\n\n[[items]]\nn = 2\n";
    let res = edit(text, json!({"items": [{"n": 10}, {"n": 20}, {"n": 30}]}));
    assert!(res.text.contains("[[items]]"));
    assert_eq!(
        res.value,
        json!({"items": [{"n": 10}, {"n": 20}, {"n": 30}]})
    );
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
}

#[test]
fn new_array_of_objects_uses_inline_array_of_inline_tables() {
    // No pre-existing array-of-tables at this key, so the fallback
    // representation (inline array of inline tables) is used.
    let text = "top = 1\n";
    let res = edit(text, json!({"items": [{"n": 1}, {"n": 2}]}));
    assert!(res.text.contains("items = ["));
    assert!(res.text.contains("{ n = 1 }") || res.text.contains("{n = 1}"));
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
    assert_eq!(res.value, json!({"top": 1, "items": [{"n": 1}, {"n": 2}]}));
}

#[test]
fn scalar_type_change_replaces_whole_item() {
    // existing "a" is a table; patch replaces it with a plain scalar.
    let text = "[a]\nx = 1\n";
    let res = edit(text, json!({"a": 42}));
    assert_eq!(res.value, json!({"a": 42}));
    assert_eq!(parse(Format::Toml, &res.text).unwrap(), res.value);
}

#[test]
fn value_and_touched_always_consistent_with_reparsed_text() {
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("a = 1\n", json!({"a": 2, "b": 3})),
        ("[a]\nx = 1\n\n[b]\ny = 2\n", json!({"a": {"x": null}, "b": {"y": 20}})),
        ("top = 1\n", json!({"nested": {"deep": {"value": true}}})),
        ("[[items]]\nn = 1\n", json!({"items": [{"n": 1}, {"n": 2}]})),
    ];
    for (text, patch) in cases {
        let res = edit(text, patch.clone());
        let reparsed = parse(Format::Toml, &res.text).unwrap();
        assert_eq!(reparsed, res.value, "mismatch for patch {patch}");
    }
}
