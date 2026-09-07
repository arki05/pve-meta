//! Access grants and the datacenter document's `scopes` map (`docs/DESIGN.md`
//! §2).
//!
//! Two grant paths are evaluated per request, entirely by the Perl API
//! layer (PVE ACL checks and looking up `scopes` are both PVE/document
//! concerns, not this crate's): full access via PVE ACLs
//! (`VM.Audit`/`VM.Config.Options`, or `Sys.Audit`/`Sys.Modify` for the
//! datacenter document), and partial access via prefix-scoped `scopes`
//! entries, keyed by authid, that apply to *every* guest document. The
//! result of that computation is a [`Grants`], which this module turns into
//! yes/no decisions ([`Grants::can_read`], [`Grants::can_write`]), the
//! prefix list for an unscoped read ([`Grants::readable_prefixes`], see
//! [`crate::view::filter`]), and the write-time enforcement
//! ([`Grants::check_write`]). [`parse_scopes`] parses the datacenter
//! document's own `scopes` map.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{self, Value};
use crate::patch::Touched;
use crate::path::Path;

/// A scope's access level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Read-only.
    Ro,
    /// Read-write.
    Rw,
}

/// One entry of a principal's scope list: a prefix, and the access it
/// grants on every guest (and the datacenter) document (`docs/DESIGN.md`
/// §2 -- scopes are not per-guest in this revision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The key-path prefix this scope covers.
    pub prefix: Path,
    /// What the scope grants on that prefix.
    pub mode: Mode,
}

/// A principal's effective access for one request, computed by the Perl API
/// layer (`docs/DESIGN.md` §3) from PVE ACLs (`full_read`/`full_write`) plus
/// the datacenter document's `scopes` (`docs/DESIGN.md` §2). Scopes are
/// additive: they never restrict a principal that already has full access.
///
/// This is exactly the `grants_json` shape crossing the Perl/Rust boundary:
/// `{"full_read":bool,"full_write":bool,"scopes":[{"prefix":"traefik","mode":"rw"}]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    /// `true` if the principal has full read access (`VM.Audit` on the
    /// guest, or `Sys.Audit` on `/` for the datacenter document).
    #[serde(default)]
    pub full_read: bool,
    /// `true` if the principal has full write access (`VM.Config.Options`
    /// on the guest, or `Sys.Modify` on `/` for the datacenter document).
    #[serde(default)]
    pub full_write: bool,
    /// Prefix-scoped grants, in addition to (never subtracted from) full
    /// access.
    #[serde(default)]
    pub scopes: Vec<Scope>,
}

impl Grants {
    /// `true` if `path` is readable: full read access, or a scope (of
    /// either mode) whose prefix covers it.
    pub fn can_read(&self, path: &Path) -> bool {
        self.full_read || self.scopes.iter().any(|s| s.prefix.is_prefix_of(path))
    }

    /// `true` if `path` is writable: full write access, or a read-write
    /// scope whose prefix covers it.
    pub fn can_write(&self, path: &Path) -> bool {
        self.full_write || self.scopes.iter().any(|s| s.mode == Mode::Rw && s.prefix.is_prefix_of(path))
    }

    /// The prefixes to union for a "no view" read (see
    /// [`crate::view::filter`]): the whole document (`[Path::root()]`) for
    /// full read access, else every scope's prefix (of either mode).
    pub fn readable_prefixes(&self) -> Vec<Path> {
        if self.full_read {
            return vec![Path::root()];
        }
        self.scopes.iter().map(|s| s.prefix.clone()).collect()
    }

    /// Checks a write's touched paths against this grant, in the order
    /// given. `Ok(())` if every one is writable; otherwise the first path
    /// that is not (for a `403` naming the offending path).
    pub fn check_write(&self, touched: &[Touched]) -> std::result::Result<(), Path> {
        for t in touched {
            if !self.can_write(&t.path) {
                return Err(t.path.clone());
            }
        }
        Ok(())
    }
}

/// Parses the datacenter document's `scopes` map: authid -> list of
/// `{prefix, mode}` (`docs/DESIGN.md` §2):
///
/// ```yaml
/// scopes:
///   svc@pve!traefik:
///     - prefix: traefik
///       mode: rw
///     - prefix: netbird
///       mode: ro
/// ```
///
/// A missing `scopes` key is an empty map, not an error. Comment keys at the
/// top of `scopes` (a bare `__`, or `<authid>__`) are ignored.
///
/// # Errors
/// [`Error::InvalidScopes`] if `scopes` (or one of its entries) does not
/// have the shape above.
pub fn parse_scopes(dc: &Value) -> Result<HashMap<String, Vec<Scope>>> {
    let mut out = HashMap::new();
    let Some(scopes_val) = dc.get("scopes") else {
        return Ok(out);
    };
    let Some(map) = scopes_val.as_object() else {
        return Err(Error::InvalidScopes("'scopes' must be a map".to_string()));
    };
    for (authid, entries) in map.iter() {
        if model::is_comment_key(authid) {
            continue;
        }
        let arr = entries
            .as_array()
            .ok_or_else(|| Error::InvalidScopes(format!("scopes.{authid} must be a list")))?;
        let mut scopes = Vec::with_capacity(arr.len());
        for (i, entry) in arr.iter().enumerate() {
            let prefix_str = entry
                .get("prefix")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidScopes(format!("scopes.{authid}.{i}: missing 'prefix'")))?;
            let prefix = Path::parse(prefix_str)
                .map_err(|_| Error::InvalidScopes(format!("scopes.{authid}.{i}: invalid prefix '{prefix_str}'")))?;
            let mode_str = entry
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidScopes(format!("scopes.{authid}.{i}: missing 'mode'")))?;
            let mode = match mode_str {
                "ro" => Mode::Ro,
                "rw" => Mode::Rw,
                other => {
                    return Err(Error::InvalidScopes(format!(
                        "scopes.{authid}.{i}: invalid mode '{other}' (expected 'ro' or 'rw')"
                    )))
                }
            };
            scopes.push(Scope { prefix, mode });
        }
        out.insert(authid.clone(), scopes);
    }
    Ok(out)
}

/// Convenience: the scopes for one authid, or an empty list if it has none.
/// Equivalent to `parse_scopes(dc)?.remove(authid).unwrap_or_default()`.
///
/// # Errors
/// [`Error::InvalidScopes`], as [`parse_scopes`].
pub fn scopes_for(dc: &Value, authid: &str) -> Result<Vec<Scope>> {
    Ok(parse_scopes(dc)?.remove(authid).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Op;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    fn touched(path: &str) -> Touched {
        Touched { path: p(path), op: Op::Set }
    }

    // -- Grants -----------------------------------------------------------

    #[test]
    fn full_access_reads_and_writes_everything() {
        let g = Grants {
            full_read: true,
            full_write: true,
            scopes: vec![],
        };
        assert!(g.can_read(&Path::root()));
        assert!(g.can_read(&p("anything.at.all")));
        assert!(g.can_write(&p("anything")));
        assert_eq!(g.readable_prefixes(), vec![Path::root()]);
    }

    #[test]
    fn no_access_by_default() {
        let g = Grants::default();
        assert!(!g.can_read(&p("a")));
        assert!(!g.can_write(&p("a")));
        assert!(g.readable_prefixes().is_empty());
    }

    #[test]
    fn ro_scope_grants_read_but_not_write() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("netbird"), mode: Mode::Ro }],
            ..Default::default()
        };
        assert!(g.can_read(&p("netbird")));
        assert!(g.can_read(&p("netbird.groups")));
        assert!(!g.can_read(&p("traefik")));
        assert!(!g.can_write(&p("netbird")));
    }

    #[test]
    fn rw_scope_grants_read_and_write_within_prefix_only() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert!(g.can_read(&p("traefik.spec.host")));
        assert!(g.can_write(&p("traefik.spec.host")));
        assert!(!g.can_write(&p("other")));
        // A scope does not grant access to its own parent/sibling paths.
        assert!(!g.can_read(&Path::root()));
    }

    #[test]
    fn readable_prefixes_lists_every_scope_regardless_of_mode() {
        let g = Grants {
            scopes: vec![
                Scope { prefix: p("traefik"), mode: Mode::Rw },
                Scope { prefix: p("netbird"), mode: Mode::Ro },
            ],
            ..Default::default()
        };
        let mut prefixes = g.readable_prefixes();
        prefixes.sort();
        assert_eq!(prefixes, vec![p("netbird"), p("traefik")]);
    }

    #[test]
    fn full_read_ignores_any_scopes_in_readable_prefixes() {
        let g = Grants {
            full_read: true,
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert_eq!(g.readable_prefixes(), vec![Path::root()]);
    }

    #[test]
    fn check_write_ok_when_every_touched_path_is_writable() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        let touched = vec![touched("traefik.spec.host"), touched("traefik.other")];
        assert_eq!(g.check_write(&touched), Ok(()));
    }

    #[test]
    fn check_write_reports_first_denied_path() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        let touched = vec![touched("traefik.spec.host"), touched("netbird.groups"), touched("also.denied")];
        assert_eq!(g.check_write(&touched), Err(p("netbird.groups")));
    }

    #[test]
    fn check_write_denies_ro_scope() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("netbird"), mode: Mode::Ro }],
            ..Default::default()
        };
        assert_eq!(g.check_write(&[touched("netbird.groups")]), Err(p("netbird.groups")));
    }

    #[test]
    fn check_write_empty_touched_is_always_ok() {
        assert_eq!(Grants::default().check_write(&[]), Ok(()));
    }

    #[test]
    fn grants_json_shape_round_trips() {
        let g = Grants {
            full_read: false,
            full_write: false,
            scopes: vec![
                Scope { prefix: p("traefik"), mode: Mode::Rw },
                Scope { prefix: p("netbird"), mode: Mode::Ro },
            ],
        };
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "full_read": false,
                "full_write": false,
                "scopes": [
                    {"prefix": "traefik", "mode": "rw"},
                    {"prefix": "netbird", "mode": "ro"},
                ],
            })
        );
        let back: Grants = serde_json::from_value(json).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn grants_json_defaults_missing_fields_to_false_and_empty() {
        let g: Grants = serde_json::from_value(json!({})).unwrap();
        assert_eq!(g, Grants::default());
    }

    // -- parse_scopes -------------------------------------------------------

    #[test]
    fn parse_scopes_missing_key_is_empty_ok() {
        assert_eq!(parse_scopes(&json!({})).unwrap(), HashMap::new());
    }

    #[test]
    fn parse_scopes_parses_the_design_doc_example() {
        let dc = json!({
            "scopes": {
                "svc@pve!traefik": [
                    {"prefix": "traefik", "mode": "rw"},
                    {"prefix": "netbird", "mode": "ro"},
                ],
            },
        });
        let parsed = parse_scopes(&dc).unwrap();
        assert_eq!(
            parsed.get("svc@pve!traefik").unwrap(),
            &vec![
                Scope { prefix: p("traefik"), mode: Mode::Rw },
                Scope { prefix: p("netbird"), mode: Mode::Ro },
            ]
        );
    }

    #[test]
    fn parse_scopes_ignores_comment_keys() {
        let dc = json!({
            "scopes": {
                "__": "who gets what",
                "svc@pve!x__": "the traefik service token",
                "svc@pve!x": [{"prefix": "traefik", "mode": "ro"}],
            },
        });
        let parsed = parse_scopes(&dc).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains_key("svc@pve!x"));
    }

    #[test]
    fn parse_scopes_rejects_non_map_scopes() {
        assert!(matches!(parse_scopes(&json!({"scopes": [1, 2]})), Err(Error::InvalidScopes(_))));
    }

    #[test]
    fn parse_scopes_rejects_non_list_entry() {
        let dc = json!({"scopes": {"a@pve": {"prefix": "x", "mode": "ro"}}});
        assert!(matches!(parse_scopes(&dc), Err(Error::InvalidScopes(_))));
    }

    #[test]
    fn parse_scopes_rejects_missing_prefix() {
        let dc = json!({"scopes": {"a@pve": [{"mode": "ro"}]}});
        assert!(matches!(parse_scopes(&dc), Err(Error::InvalidScopes(_))));
    }

    #[test]
    fn parse_scopes_rejects_invalid_prefix() {
        let dc = json!({"scopes": {"a@pve": [{"prefix": "bad key", "mode": "ro"}]}});
        assert!(matches!(parse_scopes(&dc), Err(Error::InvalidScopes(_))));
    }

    #[test]
    fn parse_scopes_rejects_missing_or_invalid_mode() {
        let dc1 = json!({"scopes": {"a@pve": [{"prefix": "x"}]}});
        assert!(matches!(parse_scopes(&dc1), Err(Error::InvalidScopes(_))));
        let dc2 = json!({"scopes": {"a@pve": [{"prefix": "x", "mode": "readwrite"}]}});
        assert!(matches!(parse_scopes(&dc2), Err(Error::InvalidScopes(_))));
    }

    #[test]
    fn parse_scopes_allows_root_prefix() {
        let dc = json!({"scopes": {"a@pve": [{"prefix": "", "mode": "ro"}]}});
        let parsed = parse_scopes(&dc).unwrap();
        assert_eq!(parsed["a@pve"], vec![Scope { prefix: Path::root(), mode: Mode::Ro }]);
    }

    #[test]
    fn scopes_for_looks_up_one_authid() {
        let dc = json!({
            "scopes": {
                "svc@pve!x": [{"prefix": "traefik", "mode": "rw"}],
            },
        });
        assert_eq!(scopes_for(&dc, "svc@pve!x").unwrap(), vec![Scope { prefix: p("traefik"), mode: Mode::Rw }]);
        assert_eq!(scopes_for(&dc, "nobody@pve").unwrap(), Vec::<Scope>::new());
    }
}
