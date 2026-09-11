//! Effective: a principal's effective access to one document
//! (`docs/DESIGN.md` §3).
//!
//! A [`Effective`] is built per request by [`crate::api`] from two inputs: the
//! PVE ACL answers Perl passes in (`full_read`/`full_write`) and the scope
//! rules of the [permission files](crate::registry) whose `authid` is the
//! caller and whose selector matches the guest. Scopes are **additive**: they
//! never restrict a principal that already holds the ACL, and they apply to
//! guest documents only — a registry document is governed by ACLs alone.
//!
//! This module turns that into yes/no decisions ([`Effective::can_read`],
//! [`Effective::can_write`]), the prefix list for an unscoped read
//! ([`Effective::readable_prefixes`], see [`crate::view::filter`]) and the
//! write-time enforcement ([`Effective::check_write`]).

use serde::{Deserialize, Serialize};

use crate::model;
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

/// One resolved scope: a key-path prefix and the access it grants on the
/// document being addressed. Selectors are already resolved by the time a
/// `Scope` exists (`docs/DESIGN.md` §5, `GET /meta/access`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The key-path prefix this scope covers.
    pub prefix: Path,
    /// What the scope grants on that prefix.
    pub mode: Mode,
}

/// A principal's effective access for one document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effective {
    /// `true` if the principal has full read access (`VM.Audit` on the
    /// guest; always true for a registry document).
    #[serde(default)]
    pub full_read: bool,
    /// `true` if the principal has full write access (`VM.Config.Options`
    /// on the guest, or `Sys.Modify` on `/` for a registry document).
    #[serde(default)]
    pub full_write: bool,
    /// Prefix-scoped permissions, in addition to (never subtracted from) full
    /// access.
    #[serde(default)]
    pub scopes: Vec<Scope>,
}

/// `true` if the scope prefix `prefix` covers `path`.
///
/// Beyond plain prefix containment, a scope on `p` also covers the sibling
/// **comment key** `p__` — the human note *about* `p`. That is the **only**
/// comment-key rule in the project (`docs/DESIGN.md` §3): a comment key is
/// otherwise ordinary data that travels with the subtree it sits in.
///
/// The aliasing is confined to the final segment at the scope's own depth, so
/// [`Path::is_prefix_of`] stays a pure structural predicate. A bare `__` (the
/// note about a whole *map*) has no subject key and is therefore covered only
/// where the map itself is.
pub fn covers(prefix: &Path, path: &Path) -> bool {
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

impl Effective {
    /// `true` if `path` is readable: full read access, or a scope (of
    /// either mode) whose prefix covers it (see [`covers`]).
    pub fn can_read(&self, path: &Path) -> bool {
        self.full_read || self.scopes.iter().any(|s| covers(&s.prefix, path))
    }

    /// `true` if `path` is writable: full write access, or a read-write
    /// scope whose prefix covers it (see [`covers`]).
    pub fn can_write(&self, path: &Path) -> bool {
        self.full_write
            || self
                .scopes
                .iter()
                .any(|s| s.mode == Mode::Rw && covers(&s.prefix, path))
    }

    /// `true` if this principal may write **something** in this document:
    /// full write access, or at least one read-write scope.
    ///
    /// Not a substitute for [`Effective::can_write`], which answers about a
    /// path. This answers the coarser question the API's request-shaped gate
    /// asks — *have you any business writing here at all* — so that a caller
    /// with no write permission whatsoever cannot reach the content check and
    /// use it as an oracle, and cannot cause the file to be rewritten by a
    /// change the content check does not measure (see
    /// `api::authorize_view_write`).
    pub fn has_any_write(&self) -> bool {
        self.full_write || self.scopes.iter().any(|s| s.mode == Mode::Rw)
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
    /// that is not (for a `403` naming it).
    pub fn check_write(&self, touched: &[Touched]) -> std::result::Result<(), Path> {
        for t in touched {
            if !self.can_write(&t.path) {
                return Err(t.path.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Op;
    use pretty_assertions::assert_eq;

    /// The prefix-coverage rule, one case per line (`docs/DESIGN.md` §3.3).
    ///
    /// This table used to be `testdata/covers-cases.json`, read by this crate and
    /// by the editor's JavaScript suite, because the editor held its own copy of
    /// `covers` to predict what the server would enforce -- and an editor that
    /// predicts differently shows rows a write then rejects. The editor asks
    /// this function now (through `crates/pve-meta-wasm`), so the table lives
    /// with the one implementation.
    #[test]
    fn covers_is_containment_plus_the_one_comment_key_alias() {
        let cases: &[(&str, &str, bool, &str)] = &[
            ("traefik", "traefik", true, "the prefix itself"),
            ("traefik", "traefik.spec", true, "a child"),
            ("traefik", "traefik.spec.host", true, "a grandchild"),
            ("traefik", "traefik__", true, "the sibling comment key travels with its subject"),
            ("traefik", "traefikx", false, "a longer name is not a child -- the separator is what makes one"),
            ("traefik", "traefikx.spec", false, "... and still is not, deeper"),
            ("traefik", "netbird", false, "unrelated"),
            ("traefik", "", false, "the root is above the prefix, not under it"),
            ("a.b", "a.b", true, "a nested prefix, exactly"),
            ("a.b", "a.b.c", true, "under a nested prefix"),
            ("a.b", "a.b__", true, "the comment key of a nested prefix"),
            ("a.b", "a.c", false, "a sibling of the nested prefix"),
            ("a.b", "a", false, "the parent is not covered by the child"),
            ("a.b", "a__", false, "nor is the parent's comment key"),
            ("a", "a.b__", true, "a comment key deeper inside the subtree"),
            ("a", "__", false, "the bare `__` documents the map it sits in, so it is readable only where that map is -- it has no subject key to travel with"),
            ("a", "a.__", true, "but the bare `__` *inside* the subtree is part of it"),
            ("homelab.docker", "homelab.docker.compose", true, "the nesting case revision 6 is about"),
            ("homelab.docker", "homelab.notes", false, "a sibling subtree of the nested prefix"),
        ];
        for (prefix, path, want, why) in cases {
            assert_eq!(
                covers(&p(prefix), &p(path)),
                *want,
                "covers({prefix:?}, {path:?}) should be {want}: {why}"
            );
        }
    }

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    fn touched(path: &str) -> Touched {
        Touched { path: p(path), op: Op::Set }
    }

    #[test]
    fn full_access_reads_and_writes_everything() {
        let g = Effective {
            full_read: true,
            full_write: true,
            scopes: vec![],
        };
        assert!(g.can_read(&Path::root()));
        assert!(g.can_read(&p("anything.at.all")));
        assert!(g.can_write(&p("anything")));
        assert_eq!(g.readable_prefixes(), vec![Path::root()]);
        assert!(!g.readable_prefixes().is_empty());
    }

    #[test]
    fn no_access_by_default() {
        let g = Effective::default();
        assert!(!g.can_read(&p("a")));
        assert!(!g.can_write(&p("a")));
        assert!(g.readable_prefixes().is_empty());
        assert!(g.readable_prefixes().is_empty());
    }

    #[test]
    fn ro_scope_permissions_read_but_not_write() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("netbird"), mode: Mode::Ro }],
            ..Default::default()
        };
        assert!(g.can_read(&p("netbird")));
        assert!(g.can_read(&p("netbird.groups")));
        assert!(!g.can_read(&p("traefik")));
        assert!(!g.can_write(&p("netbird")));
    }

    #[test]
    fn rw_scope_permissions_read_and_write_within_prefix_only() {
        let g = Effective {
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
        let g = Effective {
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
        let g = Effective {
            full_read: true,
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert_eq!(g.readable_prefixes(), vec![Path::root()]);
    }

    #[test]
    fn has_any_write_is_full_write_or_one_rw_scope() {
        assert!(!Effective::default().has_any_write());
        assert!(Effective { full_write: true, ..Default::default() }.has_any_write());
        assert!(!Effective {
            full_read: true,
            scopes: vec![Scope { prefix: p("netbird"), mode: Mode::Ro }],
            ..Default::default()
        }
        .has_any_write());
        assert!(Effective {
            scopes: vec![
                Scope { prefix: p("netbird"), mode: Mode::Ro },
                Scope { prefix: p("traefik"), mode: Mode::Rw },
            ],
            ..Default::default()
        }
        .has_any_write());
    }

    #[test]
    fn check_write_ok_when_every_touched_path_is_writable() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        let touched = vec![touched("traefik.spec.host"), touched("traefik.other")];
        assert_eq!(g.check_write(&touched), Ok(()));
    }

    #[test]
    fn check_write_reports_first_denied_path() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        let touched =
            vec![touched("traefik.spec.host"), touched("netbird.groups"), touched("also.denied")];
        assert_eq!(g.check_write(&touched), Err(p("netbird.groups")));
    }

    #[test]
    fn check_write_denies_ro_scope() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("netbird"), mode: Mode::Ro }],
            ..Default::default()
        };
        assert_eq!(g.check_write(&[touched("netbird.groups")]), Err(p("netbird.groups")));
    }

    #[test]
    fn check_write_empty_touched_is_always_ok() {
        // Vacuously true, and therefore *not* a security boundary on its
        // own: the API layer must independently require `can_write(view)`
        // before it computes anything, and `crate::view`'s operations must
        // never change a document while reporting no touched paths.
        assert_eq!(Effective::default().check_write(&[]), Ok(()));
    }

    #[test]
    fn scope_covers_the_sibling_comment_key_of_its_own_prefix() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert!(g.can_read(&p("traefik__")));
        assert!(g.can_write(&p("traefik__")));
        // Comment keys *inside* the subtree are covered by plain containment.
        assert!(g.can_write(&p("traefik.spec__")));
        assert!(g.can_write(&p("traefik.__")));
    }

    #[test]
    fn comment_key_aliasing_does_not_leak_to_other_keys() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("traefik"), mode: Mode::Rw }],
            ..Default::default()
        };
        assert!(!g.can_read(&p("netbird__")));
        assert!(!g.can_read(&p("traefikx__")));
        // The bare map comment at the document root has no subject key.
        assert!(!g.can_read(&p("__")));
        // A deeper path is not aliased up to the scope's depth.
        assert!(!g.can_read(&p("other.traefik__")));
    }

    #[test]
    fn nested_scope_covers_its_own_comment_key_at_the_same_depth() {
        let g = Effective {
            scopes: vec![Scope { prefix: p("a.b"), mode: Mode::Ro }],
            ..Default::default()
        };
        assert!(g.can_read(&p("a.b__")));
        assert!(!g.can_read(&p("a.c__")));
        assert!(!g.can_read(&p("a__")));
    }
}
