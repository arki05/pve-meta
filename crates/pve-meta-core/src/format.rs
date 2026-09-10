//! The two supported serialization formats (YAML, JSON): canonical
//! parse/dump, with format-specific validation.
//!
//! YAML is the *only* on-disk format (`docs/DESIGN.md` §2); JSON exists
//! solely as a wire format for a view's `data` (`docs/DESIGN.md` §3). There
//! is no TOML support: it was removed together with the unreachable
//! format-preserving edit engine.

use std::fmt;
use std::str::FromStr;

use saphyr_parser::{Event, Parser};

use crate::error::{Error, Location};
use crate::model::{self, Value};

/// A supported document serialization format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// YAML (block style on dump). The only on-disk format.
    Yaml,
    /// JSON (pretty-printed on dump). A wire format only.
    Json,
}

impl Format {
    /// All supported formats, in a stable order.
    pub const ALL: [Format; 2] = [Format::Yaml, Format::Json];

    /// The canonical file extension (`yaml`, `json`); never `yml`.
    pub fn ext(&self) -> &'static str {
        match self {
            Format::Yaml => "yaml",
            Format::Json => "json",
        }
    }

    /// Parses a file extension into a `Format`. Accepts `yml` as an alias for
    /// `yaml`. Case-insensitive.
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "yaml" | "yml" => Some(Format::Yaml),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.ext())
    }
}

impl FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Format::from_ext(s).ok_or_else(|| Error::InvalidName(format!("unknown format '{s}'")))
    }
}

/// Parses `text` as `format` into a [`Value`], with no [`model::lint`] pass.
///
/// Used by [`parse`] (which adds the lint), by the store's tolerant read, by
/// [`crate::view::parse`]/[`crate::view::parse_patch`] (a view's payload is
/// linted where it lands, as part of the planned document) and by
/// [`crate::registry::parse_prefix`] / [`crate::registry::parse_permission`].
///
/// # Errors
/// [`Error::Parse`] on a syntax error (or a format-specific rejection: YAML
/// anchors, aliases, explicit tags, or non-string keys).
pub(crate) fn parse_raw(format: Format, text: &str) -> Result<Value, Error> {
    match format {
        Format::Json => serde_json::from_str(text).map_err(|e| Error::Parse {
            format,
            msg: e.to_string(),
            at: Some(Location { line: e.line(), column: e.column() }),
        }),
        Format::Yaml => parse_yaml(text),
    }
}

/// Parses `text` as `format`, then runs [`model::lint`] on the result.
///
/// # Errors
/// [`Error::Parse`] on a syntax error (or a format-specific rejection: YAML
/// anchors, aliases, explicit tags, or non-string keys).
/// [`Error::Lint`] if the parsed value fails document-model validation.
pub fn parse(format: Format, text: &str) -> Result<Value, Error> {
    let value = parse_raw(format, text)?;
    let lints = model::lint(&value);
    if !lints.is_empty() {
        return Err(Error::Lint(lints));
    }
    Ok(value)
}

/// Dumps `doc` in canonical form for `format`. Always ends with a single
/// trailing newline and preserves key order. `doc` is assumed to already
/// satisfy [`model::lint`] (this function does not itself validate it).
///
/// Free-form comments in a document's previous text are *not* preserved: a
/// document is always rewritten canonically from its value. Comment *keys*
/// (`foo__`) are ordinary data and survive (`docs/DESIGN.md` §1).
pub fn dump(format: Format, doc: &Value) -> String {
    let text = match format {
        Format::Json => serde_json::to_string_pretty(doc).expect("json dump of a valid document"),
        Format::Yaml => serde_yaml_ng::to_string(doc).expect("yaml dump of a valid document"),
    };
    ensure_single_trailing_newline(text)
}

fn ensure_single_trailing_newline(mut s: String) -> String {
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

// ---------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------

fn parse_yaml(text: &str) -> Result<Value, Error> {
    scan_yaml_safety(text)?;
    serde_yaml_ng::from_str::<Value>(text).map_err(|e| Error::Parse {
        format: Format::Yaml,
        msg: e.to_string(),
        // serde_yaml_ng's `Location` is 1-based on both axes already.
        at: e.location().map(|l| Location { line: l.line(), column: l.column() }),
    })
}

/// Where a saphyr span starts, 1-based. A saphyr `Marker` is 0-based.
fn span_start(span: &saphyr_parser::Span) -> Location {
    Location { line: span.start.line() + 1, column: span.start.col() + 1 }
}

/// Context used while walking saphyr's event stream to reject anchors,
/// aliases, explicit tags and non-string (complex) mapping keys.
enum Ctx {
    Seq,
    Map { expect_key: bool },
}

fn yaml_err(msg: impl Into<String>, at: Location) -> Error {
    Error::Parse {
        format: Format::Yaml,
        msg: msg.into(),
        at: Some(at),
    }
}

fn scan_yaml_safety(text: &str) -> Result<(), Error> {
    let parser = Parser::new_from_str(text);
    let mut stack: Vec<Ctx> = Vec::new();

    // Records that a scalar/sequence/mapping is being consumed as a value
    // (toggling the enclosing map's expect_key back to `true`) or as a key
    // (toggling it to `false`, awaiting the value).
    fn note_child(stack: &mut [Ctx]) {
        if let Some(Ctx::Map { expect_key }) = stack.last_mut() {
            *expect_key = !*expect_key;
        }
    }

    fn reject_if_key_position(stack: &[Ctx], at: Location) -> Result<(), Error> {
        if let Some(Ctx::Map { expect_key: true }) = stack.last() {
            return Err(yaml_err("non-string (complex) mapping keys are not allowed", at));
        }
        Ok(())
    }

    for ev in parser {
        let (event, span) = ev.map_err(|e| {
            let m = e.marker();
            yaml_err(e.to_string(), Location { line: m.line() + 1, column: m.col() + 1 })
        })?;
        let at = span_start(&span);
        match &event {
            Event::Alias(_) => {
                return Err(yaml_err("YAML aliases are not allowed", at));
            }
            Event::Scalar(_, _, anchor, tag) => {
                if *anchor != 0 {
                    return Err(yaml_err("YAML anchors are not allowed", at));
                }
                if tag.is_some() {
                    return Err(yaml_err("YAML explicit tags are not allowed", at));
                }
                note_child(&mut stack);
            }
            Event::SequenceStart(anchor, tag) => {
                if *anchor != 0 {
                    return Err(yaml_err("YAML anchors are not allowed", at));
                }
                if tag.is_some() {
                    return Err(yaml_err("YAML explicit tags are not allowed", at));
                }
                reject_if_key_position(&stack, at)?;
                stack.push(Ctx::Seq);
            }
            Event::SequenceEnd => {
                stack.pop();
                note_child(&mut stack);
            }
            Event::MappingStart(anchor, tag) => {
                if *anchor != 0 {
                    return Err(yaml_err("YAML anchors are not allowed", at));
                }
                if tag.is_some() {
                    return Err(yaml_err("YAML explicit tags are not allowed", at));
                }
                reject_if_key_position(&stack, at)?;
                stack.push(Ctx::Map { expect_key: true });
            }
            Event::MappingEnd => {
                stack.pop();
                note_child(&mut stack);
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn format_ext_and_from_ext() {
        assert_eq!(Format::Yaml.ext(), "yaml");
        assert_eq!(Format::from_ext("yml"), Some(Format::Yaml));
        assert_eq!(Format::from_ext("YAML"), Some(Format::Yaml));
        assert_eq!(Format::from_ext("json"), Some(Format::Json));
        assert_eq!(Format::from_ext("ini"), None);
        // TOML is gone entirely (docs/DESIGN.md §2: YAML on disk).
        assert_eq!(Format::from_ext("toml"), None);
    }

    #[test]
    fn format_display_and_from_str() {
        assert_eq!(Format::Json.to_string(), "json");
        assert_eq!("json".parse::<Format>().unwrap(), Format::Json);
        assert_eq!("yml".parse::<Format>().unwrap(), Format::Yaml);
        assert!("toml".parse::<Format>().is_err());
        assert!("nope".parse::<Format>().is_err());
    }

    #[test]
    fn json_parse_and_dump() {
        let doc = json!({"b": 1, "a": 2});
        let text = dump(Format::Json, &doc);
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        let back = parse(Format::Json, &text).unwrap();
        assert_eq!(back, doc);
        // order preserved in the dumped text
        assert!(text.find("\"b\"").unwrap() < text.find("\"a\"").unwrap());
    }

    #[test]
    fn json_rejects_comments() {
        let err = parse(Format::Json, "{ \"a\": 1 /* c */ }").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
    }

    #[test]
    fn yaml_parse_and_dump_round_trip_and_order() {
        let doc = json!({"z": 1, "a": {"y": 2, "x": 3}, "list": [1, 2, 3]});
        let text = dump(Format::Yaml, &doc);
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        assert!(!text.starts_with("---"));
        let back = parse(Format::Yaml, &text).unwrap();
        assert_eq!(back, doc);
        assert!(text.find("z:").unwrap() < text.find("a:").unwrap());
    }

    #[test]
    fn yaml_1_1_words_stay_strings() {
        let doc = parse(Format::Yaml, "a: yes\nb: no\nc: on\nd: off\ne: true\nf: false\n").unwrap();
        assert_eq!(
            doc,
            json!({"a": "yes", "b": "no", "c": "on", "d": "off", "e": true, "f": false})
        );
    }

    #[test]
    fn yaml_rejects_anchors_and_aliases() {
        let err = parse(Format::Yaml, "a: &anchor 1\nb: *anchor\n").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
        assert!(err.to_string().contains("anchor"));
    }

    #[test]
    fn yaml_rejects_explicit_tags() {
        let err = parse(Format::Yaml, "a: !!str 123\n").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
        let err2 = parse(Format::Yaml, "a: !Custom 123\n").unwrap_err();
        assert!(matches!(err2, Error::Parse { .. }));
    }

    #[test]
    fn yaml_rejects_complex_keys() {
        let err = parse(Format::Yaml, "? [1, 2]\n: val\n").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
    }

    #[test]
    fn yaml_rejects_null_via_lint() {
        let err = parse(Format::Yaml, "a: ~\n").unwrap_err();
        assert!(matches!(err, Error::Lint(_)));
    }

    #[test]
    fn cross_format_preserves_order() {
        let doc = json!({"z": 1, "a": 2, "m": 3});
        for src in Format::ALL {
            let text = dump(src, &doc);
            let back = parse(src, &text).unwrap();
            assert_eq!(back, doc);
            for dst in Format::ALL {
                let converted_text = dump(dst, &back);
                let converted_val = parse(dst, &converted_text).unwrap();
                assert_eq!(converted_val, doc, "round trip {src:?} -> {dst:?} failed");
            }
        }
    }
}
