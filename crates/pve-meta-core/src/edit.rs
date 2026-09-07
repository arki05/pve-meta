//! Applying a merge patch to a document's *text*, preserving formatting for
//! TOML.

use serde_json::Map;

use crate::error::{Error, Result};
use crate::format::{self, json_to_toml_value, Format};
use crate::model::{self, Value};
use crate::patch::{self, Touched};
use crate::path::Path;

/// The result of applying a patch to a document's text.
#[derive(Debug, Clone, PartialEq)]
pub struct EditResult {
    /// The new document text.
    pub text: String,
    /// The new document value (parsed back from `text`, unstripped).
    pub value: Value,
    /// The paths that changed.
    pub touched: Vec<Touched>,
}

/// Applies `patch` (merge-patch semantics, see [`patch::apply_patch`]) to a
/// document given as `text` in `format`, returning the new text, value and
/// touched paths.
///
/// For YAML and JSON this parses, patches the value, lints, and re-dumps in
/// canonical form (comment *keys* survive; free-form comments in the original
/// text do not — this is a documented v1 limitation). For TOML the original
/// document's formatting (comments, blank lines, key style) is preserved as
/// much as possible: only the parts of the tree actually touched by the
/// patch are rewritten.
///
/// # Errors
/// [`Error::Parse`] if `text` does not parse as `format`; [`Error::Lint`] if
/// the patched document fails document-model validation.
pub fn apply_patch_text(format: Format, text: &str, patch: &Value) -> Result<EditResult> {
    match format {
        Format::Toml => apply_patch_toml(text, patch),
        Format::Yaml | Format::Json => {
            let mut value = format::parse(format, text)?;
            let touched = patch::apply_patch(&mut value, patch);
            let lints = model::lint(&value);
            if !lints.is_empty() {
                return Err(Error::Lint(lints));
            }
            let out_text = format::dump(format, &value);
            Ok(EditResult {
                text: out_text,
                value,
                touched,
            })
        }
    }
}

fn apply_patch_toml(text: &str, patch: &Value) -> Result<EditResult> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        Error::Parse {
            format: Format::Toml,
            msg: e.to_string(),
        }
    })?;

    // The tree edit (format-preserving).
    if let Some(patch_obj) = patch.as_object() {
        apply_patch_table(doc.as_table_mut(), patch_obj);
    }
    let out_text = doc.to_string();

    // `value` is derived from the *actually edited* text, so it is
    // guaranteed consistent with it (including TOML-specific structural
    // quirks such as a fully-emptied table's header disappearing, which the
    // generic, format-agnostic `patch::apply_patch` below does not model:
    // it leaves a merge-patch-emptied object as `{}` rather than pruning it).
    let value = format::parse(Format::Toml, &out_text)?;

    // `touched` is computed independently, by applying the same patch to the
    // value parsed from the *original* text, per spec.
    let mut original_value = format::parse(Format::Toml, text)?;
    let touched = patch::apply_patch(&mut original_value, patch);

    Ok(EditResult {
        text: out_text,
        value,
        touched,
    })
}

/// Recursively applies a patch object onto a live `toml_edit` table.
fn apply_patch_table(table: &mut toml_edit::Table, patch: &Map<String, Value>) {
    for (k, v) in patch.iter() {
        match v {
            Value::Null => {
                table.remove(k);
            }
            Value::Object(sub) => {
                let recurse_existing = matches!(table.get(k), Some(toml_edit::Item::Table(_)));
                if recurse_existing {
                    if let Some(toml_edit::Item::Table(t)) = table.get_mut(k) {
                        apply_patch_table(t, sub);
                        if t.is_empty() {
                            table.remove(k);
                        }
                    }
                } else {
                    let mut new_table = toml_edit::Table::new();
                    new_table.set_implicit(false);
                    insert_new_object(&mut new_table, sub);
                    table.insert(k, toml_edit::Item::Table(new_table));
                }
            }
            Value::Array(items)
                if !items.is_empty()
                    && items.iter().all(Value::is_object)
                    && matches!(table.get(k), Some(toml_edit::Item::ArrayOfTables(_))) =>
            {
                let mut aot = toml_edit::ArrayOfTables::new();
                for item in items {
                    let mut t = toml_edit::Table::new();
                    insert_new_object(&mut t, item.as_object().expect("checked all-object above"));
                    aot.push(t);
                }
                table.insert(k, toml_edit::Item::ArrayOfTables(aot));
            }
            _ => set_scalar_or_array(table, k, v),
        }
    }
}

/// Sets a scalar/array value at `k`, preserving the existing value's decor
/// (surrounding whitespace and trailing comment) when replacing an existing
/// plain value in place; otherwise inserts a fresh key.
fn set_scalar_or_array(table: &mut toml_edit::Table, k: &str, v: &Value) {
    let path = Path::root().join(k.to_string());
    let new_value =
        json_to_toml_value(v, &path).unwrap_or_else(|_| toml_edit::Value::from(false));
    if let Some(toml_edit::Item::Value(existing)) = table.get_mut(k) {
        let prefix = existing.decor().prefix().cloned();
        let suffix = existing.decor().suffix().cloned();
        let mut replacement = new_value;
        if let Some(p) = prefix {
            replacement.decor_mut().set_prefix(p);
        }
        if let Some(s) = suffix {
            replacement.decor_mut().set_suffix(s);
        }
        *existing = replacement;
    } else {
        table.insert(k, toml_edit::Item::Value(new_value));
    }
}

/// Fills a brand-new (empty) table from a patch object: every key present
/// becomes a real entry (nested objects become nested explicit tables; `null`
/// entries are simply omitted, since there is nothing to delete).
fn insert_new_object(table: &mut toml_edit::Table, obj: &Map<String, Value>) {
    for (k, v) in obj.iter() {
        match v {
            Value::Null => {}
            Value::Object(sub) => {
                let mut t = toml_edit::Table::new();
                t.set_implicit(false);
                insert_new_object(&mut t, sub);
                table.insert(k, toml_edit::Item::Table(t));
            }
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
                let mut aot = toml_edit::ArrayOfTables::new();
                for item in items {
                    let mut t = toml_edit::Table::new();
                    insert_new_object(&mut t, item.as_object().expect("checked all-object above"));
                    aot.push(t);
                }
                table.insert(k, toml_edit::Item::ArrayOfTables(aot));
            }
            _ => {
                let path = Path::root().join(k.clone());
                let tv = json_to_toml_value(v, &path).unwrap_or_else(|_| toml_edit::Value::from(false));
                table.insert(k, toml_edit::Item::Value(tv));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn yaml_edit_round_trips() {
        let text = "a: 1\nb: 2\n";
        let patch = json!({"a": 10, "c": 3});
        let res = apply_patch_text(Format::Yaml, text, &patch).unwrap();
        assert_eq!(res.value, json!({"a": 10, "b": 2, "c": 3}));
        assert_eq!(res.touched.len(), 2);
        let reparsed = format::parse(Format::Yaml, &res.text).unwrap();
        assert_eq!(reparsed, res.value);
    }

    #[test]
    fn json_edit_round_trips() {
        let text = "{\n  \"a\": 1\n}\n";
        let patch = json!({"a": null, "b": 2});
        let res = apply_patch_text(Format::Json, text, &patch).unwrap();
        assert_eq!(res.value, json!({"b": 2}));
        let reparsed = format::parse(Format::Json, &res.text).unwrap();
        assert_eq!(reparsed, res.value);
    }

    #[test]
    fn toml_edit_changes_only_touched_value() {
        let text = "# header comment\ntitle = \"orig\" # trailing\nother = 2\n";
        let patch = json!({"title": "new"});
        let res = apply_patch_text(Format::Toml, text, &patch).unwrap();
        assert_eq!(
            res.text,
            "# header comment\ntitle = \"new\" # trailing\nother = 2\n"
        );
        assert_eq!(res.value, json!({"title": "new", "other": 2}));
        let reparsed = format::parse(Format::Toml, &res.text).unwrap();
        assert_eq!(reparsed, res.value);
    }

    #[test]
    fn toml_edit_appends_new_key() {
        let text = "a = 1\n";
        let patch = json!({"b": 2});
        let res = apply_patch_text(Format::Toml, text, &patch).unwrap();
        assert_eq!(res.text, "a = 1\nb = 2\n");
    }

    #[test]
    fn toml_edit_creates_new_table() {
        let text = "a = 1\n";
        let patch = json!({"nested": {"x": 1, "y": 2}});
        let res = apply_patch_text(Format::Toml, text, &patch).unwrap();
        assert_eq!(res.text, "a = 1\n\n[nested]\nx = 1\ny = 2\n");
        let reparsed = format::parse(Format::Toml, &res.text).unwrap();
        assert_eq!(reparsed, res.value);
    }

    #[test]
    fn toml_edit_deletes_key_and_empty_table_header() {
        let text = "[a]\nx = 1\n\n[b]\ny = 2\n";
        let patch = json!({"a": {"x": null}});
        let res = apply_patch_text(Format::Toml, text, &patch).unwrap();
        // The blank line before `[b]` is `b`'s own leading decor, untouched
        // by removing `a` entirely (including its header).
        assert_eq!(res.text, "\n[b]\ny = 2\n");
        assert_eq!(res.value, json!({"b": {"y": 2}}));
    }
}
