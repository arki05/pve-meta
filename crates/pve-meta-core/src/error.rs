//! The single error type used throughout `pve-meta-core`.

use thiserror::Error;

use crate::format::Format;
use crate::model::Lint;
use crate::store::DocId;

/// Convenience alias for `Result<T, Error>`, used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A position in a document's text, 1-based on both axes, as a parser
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// All errors that can be produced by `pve-meta-core`.
///
/// `Display` messages are written to be suitable for direct use as an HTTP API
/// error response body.
#[derive(Debug, Error)]
pub enum Error {
    /// A document's text could not be parsed as the given [`Format`].
    #[error("failed to parse {format} document: {msg}")]
    Parse {
        /// The format that failed to parse.
        format: Format,
        /// A human-readable description of the failure.
        msg: String,
        /// Where in the text, when the parser knows. An editor puts its
        /// marker on this line; the message alone would leave it guessing.
        at: Option<Location>,
    },

    /// The document failed the lint (see [`crate::model::lint`]).
    #[error("document failed validation: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Lint(Vec<Lint>),

    /// An optimistic-concurrency check failed: the caller's expected digest did
    /// not match the document's actual current digest.
    #[error("digest mismatch: expected {expected}, actual {actual}")]
    DigestMismatch {
        /// The digest the caller expected.
        expected: String,
        /// The document's actual digest.
        actual: String,
    },

    /// No document exists for the given id.
    #[error("not found: {0}")]
    NotFound(DocId),

    /// The document (or patch result) exceeds the configured maximum size.
    #[error("document too large: {size} bytes (max {max} bytes)")]
    TooLarge {
        /// The offending size, in bytes.
        size: u64,
        /// The configured maximum, in bytes.
        max: u64,
    },

    /// A [`crate::path::Path`] string could not be parsed, or is otherwise invalid.
    #[error("invalid path: {0}")]
    InvalidPath(String),

    /// An identifier (e.g. a snapshot name or format name) is invalid.
    #[error("invalid name: {0}")]
    InvalidName(String),

    /// A prefix definition or permission file (`docs/DESIGN.md` §6) is malformed; see
    /// [`crate::registry::parse_prefix`] / [`crate::registry::parse_permission`].
    #[error("invalid registry file: {0}")]
    Registry(String),

    /// The store's filesystem is not there to answer: `/etc/pve` without
    /// pmxcfs mounted on it is an empty directory, and "no documents" read off
    /// it is a wrong answer, not an empty one (`docs/DESIGN.md` §7). See
    /// [`crate::store::MetaStore::check_available`].
    #[error("cluster filesystem not available: {0}")]
    Unavailable(String),

    /// An underlying I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Any other unexpected failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
