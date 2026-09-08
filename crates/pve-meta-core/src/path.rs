//! Dotted/slash addressing into a [document](crate::model::Value).

use std::fmt;
use std::str::FromStr;

use crate::error::Error;

/// Returns `true` if `s` is a syntactically valid path/object-key segment:
/// `^[A-Za-z0-9_@!-]+$`. `@` and `!` are allowed (in addition to the base
/// `^[A-Za-z0-9_-]+$` object-key charset) specifically so a PVE authid
/// (`user@realm`, or `user@realm!tokenid`) can be used verbatim as a key.
/// No key is reserved (`docs/DESIGN.md` §2), so an authid-shaped key is
/// ordinary document data -- revision 4's reserved, authid-keyed `scopes`
/// map is gone, and access-control data lives outside documents entirely
/// (§3). Neither character is a path separator (those are `.` and `/`), so
/// this does not introduce any addressing ambiguity.
pub(crate) fn is_valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '@' || c == '!')
}

/// A path into a document: a sequence of object-key or array-index segments.
///
/// The empty path (`Path::root()`) addresses the document root. `Display`
/// renders the path in dotted form (`a.b.c`); [`Path::parse`] accepts both
/// dotted (`a.b.c`) and slash (`a/b/c`, `/a/b/c`) forms.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Path(Vec<String>);

impl Path {
    /// The empty path, addressing the document root.
    pub fn root() -> Self {
        Path(Vec::new())
    }

    /// Builds a path from already-validated segments.
    pub fn new(segments: Vec<String>) -> Self {
        Path(segments)
    }

    /// Parses a dotted (`a.b.c`) or slash-separated (`a/b/c`, `/a/b/c`) path
    /// string. An empty string (or `/`) parses to the root path.
    ///
    /// # Errors
    /// Returns [`Error::InvalidPath`] if any segment fails validation.
    pub fn parse(s: &str) -> Result<Self, Error> {
        let trimmed = s.strip_prefix('/').unwrap_or(s);
        if trimmed.is_empty() {
            return Ok(Path::root());
        }
        let sep = if s.contains('/') { '/' } else { '.' };
        let mut segments = Vec::new();
        for part in trimmed.split(sep) {
            if !is_valid_segment(part) {
                return Err(Error::InvalidPath(s.to_string()));
            }
            segments.push(part.to_string());
        }
        Ok(Path(segments))
    }

    /// `true` if this is the root (empty) path.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The path's segments, in order.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// The last segment, if any.
    pub fn last(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }

    /// Appends a segment in place.
    pub fn push(&mut self, segment: impl Into<String>) {
        self.0.push(segment.into());
    }

    /// The parent path (all but the last segment), or `None` for the root.
    pub fn parent(&self) -> Option<Path> {
        if self.0.is_empty() {
            None
        } else {
            let mut segments = self.0.clone();
            segments.pop();
            Some(Path(segments))
        }
    }

    /// Returns a new path with `segment` appended.
    pub fn join(&self, segment: impl Into<String>) -> Path {
        let mut segments = self.0.clone();
        segments.push(segment.into());
        Path(segments)
    }

    /// `true` if `self` is a prefix of `other` (the root is a prefix of
    /// everything, including itself).
    pub fn is_prefix_of(&self, other: &Path) -> bool {
        self.0.len() <= other.0.len() && self.0.iter().zip(other.0.iter()).all(|(a, b)| a == b)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join("."))
    }
}

impl FromStr for Path {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Path::parse(s)
    }
}

/// Serializes as the dotted string form (`Display`), e.g. for a
/// [`crate::scopes::Scope`]'s `prefix` in the `grants_json` wire shape
/// (`{"prefix":"traefik","mode":"rw"}`).
impl serde::Serialize for Path {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Deserializes from the dotted/slash string form via [`Path::parse`].
impl<'de> serde::Deserialize<'de> for Path {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Path::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_empty() {
        let p = Path::root();
        assert!(p.is_root());
        assert_eq!(p.to_string(), "");
        assert_eq!(p.segments().len(), 0);
    }

    #[test]
    fn parse_dotted() {
        let p = Path::parse("a.b.c").unwrap();
        assert_eq!(p.segments(), &["a", "b", "c"]);
        assert_eq!(p.to_string(), "a.b.c");
    }

    #[test]
    fn parse_slash_forms() {
        assert_eq!(Path::parse("/a/b/c").unwrap().segments(), &["a", "b", "c"]);
        assert_eq!(Path::parse("a/b/c").unwrap().segments(), &["a", "b", "c"]);
        assert!(Path::parse("/").unwrap().is_root());
        assert!(Path::parse("").unwrap().is_root());
    }

    #[test]
    fn parse_rejects_invalid_segment() {
        assert!(Path::parse("a.b..c").is_err());
        assert!(Path::parse("a.b c").is_err());
        assert!(Path::parse("a.b.").is_err());
    }

    #[test]
    fn authid_shaped_segments_allowed() {
        // `@` and `!` are allowed so a PVE authid can be used as a segment
        // of an ordinary document key (`docs/DESIGN.md` §2: no key is
        // reserved), and are not path separators (those are `.` and `/`).
        assert!(is_valid_segment("svc@pve!traefik"));
        assert!(is_valid_segment("scoped@pve"));
        let p = Path::parse("scopes/svc@pve!traefik").unwrap();
        assert_eq!(p.segments(), &["scopes", "svc@pve!traefik"]);
    }

    #[test]
    fn numeric_segments_allowed() {
        let p = Path::parse("a.0.b").unwrap();
        assert_eq!(p.segments(), &["a", "0", "b"]);
    }

    #[test]
    fn is_prefix_of() {
        let root = Path::root();
        let a = Path::parse("a").unwrap();
        let ab = Path::parse("a.b").unwrap();
        let ac = Path::parse("a.c").unwrap();
        assert!(root.is_prefix_of(&ab));
        assert!(root.is_prefix_of(&root));
        assert!(a.is_prefix_of(&ab));
        assert!(!ab.is_prefix_of(&a));
        assert!(!ac.is_prefix_of(&ab));
        assert!(ab.is_prefix_of(&ab));
    }

    #[test]
    fn serde_round_trips_through_the_dotted_string_form() {
        let p = Path::parse("a.b.c").unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"a.b.c\"");
        let back: Path = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);

        let root_json = serde_json::to_string(&Path::root()).unwrap();
        assert_eq!(root_json, "\"\"");
        assert_eq!(serde_json::from_str::<Path>(&root_json).unwrap(), Path::root());

        assert!(serde_json::from_str::<Path>("\"a..b\"").is_err());
    }

    #[test]
    fn push_join_parent_last() {
        let mut p = Path::root();
        p.push("a");
        p.push("b");
        assert_eq!(p.to_string(), "a.b");
        assert_eq!(p.last(), Some("b"));
        let joined = p.join("c");
        assert_eq!(joined.to_string(), "a.b.c");
        assert_eq!(joined.parent().unwrap().to_string(), "a.b");
        assert_eq!(Path::root().parent(), None);
    }
}
