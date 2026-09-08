//! The single error type used throughout `pve-meta-core`.

use thiserror::Error;

use crate::format::Format;
use crate::model::Lint;
use crate::store::DocId;

/// Convenience alias for `Result<T, Error>`, used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

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
    },

    /// The document model failed one or more lint rules (see [`crate::model::lint`]).
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

    // There is no `Conflict` variant: it described "more than one format
    // file exists for the same document id", which the YAML-only store
    // (`docs/DESIGN.md` §8) cannot produce. It was never constructed after
    // the multi-format store was removed, so it went with it rather than
    // staying as a 409 arm nothing can reach (review §5).
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

    /// The datacenter document's `scopes` map (or one of its entries) does
    /// not have the shape [`crate::scopes::parse_scopes`] expects.
    #[error("invalid scopes: {0}")]
    InvalidScopes(String),

    /// An underlying I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Any other unexpected failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
