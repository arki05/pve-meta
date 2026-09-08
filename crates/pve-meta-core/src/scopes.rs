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

/// `true` if the scope prefix `prefix` covers `path`.
///
/// Beyond plain prefix containment, a scope on `p` also covers the sibling
/// **comment key** `p__` — the human note *about* `p` (`docs/DESIGN.md` §1,
/// §8: "comment keys follow their subject"). Without this a scoped principal
/// could read the note through an unscoped read (where
/// [`crate::view::filter`] brings comment keys along with their subject) but
/// never write it, and no request could ever address it.
///
/// The aliasing is deliberately confined to the *final* segment at the
/// scope's own depth: `p__` is a leaf string, so nothing can live under it,
/// and `Path::is_prefix_of` stays a pure structural predicate (review F11).
/// A bare `__` (the note about a whole *map*) has no subject key and is
/// therefore covered only where the map itself is — which is exactly the rule
/// [`crate::view::filter`] enforces on the read side (`docs/DESIGN.md` §9,
/// review P4).
pub(crate) fn covers(prefix: &Path, path: &Path) -> bool {
    if prefix.is_prefix_of(path) {
        return true;
    }
    if path.segments().len() != prefix.segments().len() {
        return false;
    }
    let Some(last) = path.last() else {
        return false;
    };
    // The bare `__` map comment has no subject key, so it is not aliased.
    let Some(base) = last
        .strip_suffix(model::COMMENT_SUFFIX)
        .filter(|b| !b.is_empty())
    else {
        return false;
    };
    let mut segments = path.segments().to_vec();
    let last_idx = segments.len() - 1;
    segments[last_idx] = base.to_string();
    prefix.is_prefix_of(&Path::new(segments))
}

impl Grants {
    /// `true` if `path` is readable: full read access, or a scope (of
    /// either mode) whose prefix covers it (see `covers`).
    pub fn can_read(&self, path: &Path) -> bool {
        self.full_read || self.scopes.iter().any(|s| covers(&s.prefix, path))
    }

    /// `true` if `path` is writable: full write access, or a read-write
    /// scope whose prefix covers it (see `covers`).
    pub fn can_write(&self, path: &Path) -> bool {
        self.full_write
            || self
                .scopes
                .iter()
                .any(|s| s.mode == Mode::Rw && covers(&s.prefix, path))
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
    ///
    /// The `scopes` map is treated as **one path** (`docs/DESIGN.md` §9): a
    /// touched path inside it is checked — and reported — as `scopes` itself,
    /// so writing any part of the access-control map needs write access to
    /// the whole of it, and a 403 never names another principal's authid.
    pub fn check_write(&self, touched: &[Touched]) -> std::result::Result<(), Path> {
        for t in touched {
            let path = opaque_scopes_path(&t.path);
            if !self.can_write(&path) {
                return Err(path);
            }
        }
        Ok(())
    }
}

/// `scopes.<authid>[...]` collapsed to `scopes`; every other path unchanged
/// (`docs/DESIGN.md` §9: the `scopes` map is an opaque leaf).
fn opaque_scopes_path(path: &Path) -> Path {
    if path.segments().len() > 1 && path.segments()[0] == model::SCOPES_KEY {
        Path::new(vec![model::SCOPES_KEY.to_string()])
    } else {
        path.clone()
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
/// This is the **strict** parse, used at *write* time: a write that touches
/// `scopes` is rejected with the offending entry named, so a malformed entry
/// can never reach the disk (`docs/DESIGN.md` §8, review F7). Read paths use
/// [`scopes_for`], which never fails at all.
///
/// Keys must be PVE authids ([`model::is_authid`]) rather than plain document
/// keys, and a prefix must be **non-empty** and must not address inside
/// `scopes` itself (`docs/DESIGN.md` §9): full access comes from PVE ACLs
/// only, and the access-control map is an opaque leaf.
///
/// # Errors
/// [`Error::InvalidScopes`] if `scopes` (or one of its keys or entries) does
/// not have the shape above.
pub fn parse_scopes(dc: &Value) -> Result<HashMap<String, Vec<Scope>>> {
    let mut out = HashMap::new();
    let Some(scopes_val) = dc.get(model::SCOPES_KEY) else {
        return Ok(out);
    };
    let Some(map) = scopes_val.as_object() else {
        return Err(Error::InvalidScopes("'scopes' must be a map".to_string()));
    };
    for (authid, entries) in map.iter() {
        if model::is_comment_key(authid) {
            continue;
        }
        if !model::is_authid(authid) {
            return Err(Error::InvalidScopes(format!(
                "scopes.{authid}: not a valid PVE authid (user@realm, optionally !tokenid)"
            )));
        }
        out.insert(authid.clone(), parse_entry(authid, entries)?);
    }
    Ok(out)
}

/// Parses one `scopes.<authid>` entry.
fn parse_entry(authid: &str, entries: &Value) -> Result<Vec<Scope>> {
    let arr = entries
        .as_array()
        .ok_or_else(|| Error::InvalidScopes(format!("scopes.{authid} must be a list")))?;
    let mut scopes = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let prefix_str = entry
            .get("prefix")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidScopes(format!("scopes.{authid}.{i}: missing 'prefix'")))?;
        let prefix = Path::parse(prefix_str).map_err(|_| {
            Error::InvalidScopes(format!("scopes.{authid}.{i}: invalid prefix '{prefix_str}'"))
        })?;
        // An empty prefix would be "everything below the root", i.e. write
        // access to every document including `scopes` itself. Full access is
        // granted through PVE ACLs and nowhere else (`docs/DESIGN.md` §9,
        // review P1).
        if prefix.is_root() {
            return Err(Error::InvalidScopes(format!(
                "scopes.{authid}.{i}: 'prefix' must not be empty (a scope covers a key-path \
                 prefix; whole-document access comes from PVE ACLs, not from a scope)"
            )));
        }
        // `scopes` is an opaque leaf: a scope may cover the whole map, never
        // one entry of it (`docs/DESIGN.md` §9, review P9).
        if prefix.segments().len() > 1 && prefix.segments()[0] == model::SCOPES_KEY {
            return Err(Error::InvalidScopes(format!(
                "scopes.{authid}.{i}: invalid prefix '{prefix_str}': the 'scopes' map is \
                 addressed as a whole, never one entry of it"
            )));
        }
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
    Ok(scopes)
}

/// The scopes for one authid, or an empty list if it has none.
///
/// **A read never fails and never lints** (`docs/DESIGN.md` §9, review
/// P2/P3): a `scopes` value that is not a map, a malformed entry (whoever it
/// belongs to), and an entry that is syntactically fine but not permitted —
/// an empty prefix, or one addressing inside `scopes` — are each skipped with
/// a `warn!` and simply grant nothing. Only [`parse_scopes`], the write gate,
/// rejects them, which is where a misconfiguration can still be fixed.
///
/// This is deliberately the *whole* lookup path's failure policy: `scopes`
/// lives inside the datacenter document and is consulted on every guest
/// request, so any error here is an outage for every principal cluster-wide,
/// full-ACL administrators included — including the administrator trying to
/// repair it (review F7, and its residue P2/P3).
pub fn scopes_for(dc: &Value, authid: &str) -> Vec<Scope> {
    let Some(scopes_val) = dc.get(model::SCOPES_KEY) else {
        return Vec::new();
    };
    let Some(map) = scopes_val.as_object() else {
        tracing::warn!("ignoring 'scopes': not a map (it grants nothing until repaired)");
        return Vec::new();
    };
    // Warn about (and ignore) anything that is broken, so an operator still
    // learns about it from the logs.
    let mut mine = Vec::new();
    for (other, entries) in map.iter() {
        if model::is_comment_key(other) {
            continue;
        }
        let parsed = if model::is_authid(other) {
            parse_entry(other, entries)
        } else {
            Err(Error::InvalidScopes(format!(
                "scopes.{other}: not a valid PVE authid"
            )))
        };
        match parsed {
            Ok(scopes) if other == authid => mine = scopes,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(scopes_entry = %other, error = %e, "ignoring malformed scopes entry");
            }
        }
    }
    mine
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
        // Vacuously true, and therefore *not* a security boundary on its
        // own: the API layer must independently require `can_write(view)`
        // before it computes anything (`docs/DESIGN.md` §8, review F1), and
        // `crate::view`'s operations must never change a document while
        // reporting no touched paths (review F2).
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
                "svc@pve!tok__": "the traefik service token",
                "svc@pve!tok": [{"prefix": "traefik", "mode": "ro"}],
            },
        });
        let parsed = parse_scopes(&dc).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains_key("svc@pve!tok"));
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
    fn parse_scopes_rejects_an_empty_prefix() {
        // `docs/DESIGN.md` §9, review P1 + LOW #1: an empty prefix is
        // "everything", i.e. write access to `scopes` itself. Whole-document
        // access is a PVE ACL, never a scope.
        for mode in ["ro", "rw"] {
            let dc = json!({"scopes": {"a@pve": [{"prefix": "", "mode": mode}]}});
            let err = parse_scopes(&dc).unwrap_err();
            assert!(err.to_string().contains("must not be empty"), "{err}");
        }
        // ... and the lenient read grants nothing rather than failing.
        let dc = json!({"scopes": {"a@pve": [{"prefix": "", "mode": "rw"}]}});
        assert_eq!(scopes_for(&dc, "a@pve"), Vec::<Scope>::new());
    }

    #[test]
    fn parse_scopes_rejects_a_prefix_inside_the_scopes_map() {
        // `docs/DESIGN.md` §9: `scopes` is an opaque leaf. A scope may cover
        // the whole map (that is still admin-only for writes, see the api
        // layer) but never one entry of it.
        let dc = json!({"scopes": {"a@pve": [{"prefix": "scopes.b@pve", "mode": "ro"}]}});
        let err = parse_scopes(&dc).unwrap_err();
        assert!(err.to_string().contains("as a whole"), "{err}");
        assert_eq!(scopes_for(&dc, "a@pve"), Vec::<Scope>::new());

        // The whole map is a legal prefix.
        let whole = json!({"scopes": {"a@pve": [{"prefix": "scopes", "mode": "rw"}]}});
        assert_eq!(
            parse_scopes(&whole).unwrap()["a@pve"],
            vec![Scope { prefix: p("scopes"), mode: Mode::Rw }]
        );
    }

    #[test]
    fn parse_scopes_requires_authid_shaped_keys_and_allows_dots() {
        // Review P9: `john.doe@pve` is a perfectly ordinary PVE authid.
        let dc = json!({"scopes": {"john.doe@ldap.corp!tok": [{"prefix": "traefik", "mode": "rw"}]}});
        assert_eq!(
            parse_scopes(&dc).unwrap()["john.doe@ldap.corp!tok"],
            vec![Scope { prefix: p("traefik"), mode: Mode::Rw }]
        );
        assert_eq!(
            scopes_for(&dc, "john.doe@ldap.corp!tok"),
            vec![Scope { prefix: p("traefik"), mode: Mode::Rw }]
        );

        let bogus = json!({"scopes": {"not-an-authid": [{"prefix": "traefik", "mode": "rw"}]}});
        let err = parse_scopes(&bogus).unwrap_err();
        assert!(err.to_string().contains("not a valid PVE authid"), "{err}");
        assert_eq!(scopes_for(&bogus, "not-an-authid"), Vec::<Scope>::new());
    }

    #[test]
    fn scopes_for_looks_up_one_authid() {
        let dc = json!({
            "scopes": {
                "svc@pve!tok": [{"prefix": "traefik", "mode": "rw"}],
            },
        });
        assert_eq!(scopes_for(&dc, "svc@pve!tok"), vec![Scope { prefix: p("traefik"), mode: Mode::Rw }]);
        assert_eq!(scopes_for(&dc, "nobody@pve"), Vec::<Scope>::new());
    }

    // -- `scopes` is one path for write checks (docs/DESIGN.md §9) ----------

    #[test]
    fn check_write_treats_the_scopes_map_as_a_single_path() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("scopes"), mode: Mode::Rw }],
            ..Default::default()
        };
        // A scope on the whole map covers a write to any entry ...
        assert_eq!(g.check_write(&[touched("scopes.a@pve"), touched("scopes.b@pve.0.mode")]), Ok(()));

        // ... and a grant that does not cover `scopes` is refused naming the
        // map, never another principal's authid.
        let other = Grants {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert_eq!(other.check_write(&[touched("scopes.evil@pve")]), Err(p("scopes")));
    }

    // -- lenient reads (review F7) ------------------------------------------

    #[test]
    fn scopes_for_skips_another_principals_malformed_entry() {
        // The outage in review F7: one bad entry used to 400 every guest
        // read and write, for every principal, cluster-wide.
        let dc = json!({
            "scopes": {
                "broken@pve": "not a list",
                "also-broken@pve": [{"prefix": "x", "mode": "readwrite"}],
                "missing-mode@pve": [{"prefix": "x"}],
                "good@pve!tok": [{"prefix": "traefik", "mode": "rw"}],
            },
        });
        assert_eq!(
            scopes_for(&dc, "good@pve!tok"),
            vec![Scope { prefix: p("traefik"), mode: Mode::Rw }]
        );
        // A full-ACL admin with no entry of their own is likewise unaffected.
        assert_eq!(scopes_for(&dc, "root@pam"), Vec::<Scope>::new());
        // ... while the strict, write-time parse still rejects the document.
        assert!(matches!(parse_scopes(&dc), Err(Error::InvalidScopes(_))));
    }

    #[test]
    fn scopes_for_skips_the_callers_own_malformed_entry_too() {
        // Review P3's philosophy taken to the end (`docs/DESIGN.md` §9): a
        // read never denies service, not even to the principal whose own
        // entry is broken -- they keep whatever their PVE ACLs give them.
        let dc = json!({"scopes": {"me@pve": [{"prefix": "x", "mode": "nope"}]}});
        assert_eq!(scopes_for(&dc, "me@pve"), Vec::<Scope>::new());
        // The write gate still names it, which is what makes it fixable.
        let err = parse_scopes(&dc).unwrap_err();
        assert!(err.to_string().contains("me@pve"), "{err}");
    }

    #[test]
    fn scopes_for_ignores_a_non_map_scopes_key_instead_of_failing() {
        // Review P3 (F7 residue): `scopes` present but not an object used to
        // 400 every guest endpoint for every caller, admins included -- one
        // indentation slip away in hand-edited YAML.
        for broken in [json!([1, 2]), json!("oops"), json!(7)] {
            let dc = json!({"scopes": broken});
            assert_eq!(scopes_for(&dc, "me@pve"), Vec::<Scope>::new());
            // The strict, write-time parse still rejects the same document.
            assert!(matches!(parse_scopes(&dc), Err(Error::InvalidScopes(_))));
        }
    }

    // -- comment keys follow their subject (review F11) ---------------------

    #[test]
    fn scope_covers_the_sibling_comment_key_of_its_own_prefix() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert!(g.can_read(&p("traefik__")));
        assert!(g.can_write(&p("traefik__")));
        // Comment keys *inside* the subtree were always covered.
        assert!(g.can_write(&p("traefik.spec__")));
        assert!(g.can_write(&p("traefik.__")));
    }

    #[test]
    fn comment_key_aliasing_does_not_leak_to_other_keys() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        // Not the scope's own comment key.
        assert!(!g.can_read(&p("netbird__")));
        assert!(!g.can_read(&p("traefikx__")));
        // The bare map comment at the document root has no subject key.
        assert!(!g.can_read(&p("__")));
        // A deeper path is not aliased up to the scope's depth.
        assert!(!g.can_read(&p("other.traefik__")));
    }

    #[test]
    fn nested_scope_covers_its_own_comment_key_at_the_same_depth() {
        let g = Grants {
            scopes: vec![Scope { prefix: p("a.b"), mode: Mode::Ro }],
            ..Default::default()
        };
        assert!(g.can_read(&p("a.b__")));
        assert!(!g.can_read(&p("a.c__")));
        assert!(!g.can_read(&p("a__")));
    }
}
