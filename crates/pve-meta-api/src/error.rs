//! Maps [`pve_meta_core::Error`] onto HTTP status codes.

use anyhow::Error;
use proxmox_router::http_err;

/// Converts a `pve-meta-core` error into an `anyhow::Error` wrapping a
/// [`proxmox_router::HttpError`], so that `proxmox-rest-server` reports the right HTTP status.
///
/// | core error | status |
/// |---|---|
/// | `DigestMismatch` | 409 Conflict |
/// | `Conflict` | 409 Conflict |
/// | `NotFound` | 404 Not Found |
/// | `Lint` / `Parse` / `InvalidPath` / `InvalidName` / `TooLarge` | 400 Bad Request |
/// | everything else (`Io`, `Other`) | 500 Internal Server Error |
pub fn to_http(err: pve_meta_core::Error) -> Error {
    use pve_meta_core::Error as E;
    match err {
        E::DigestMismatch { expected, actual } => http_err!(
            CONFLICT,
            "digest mismatch: expected {expected}, actual {actual}"
        ),
        E::Conflict(msg) => http_err!(CONFLICT, "{msg}"),
        E::NotFound(id) => http_err!(NOT_FOUND, "not found: {id}"),
        E::Lint(_) | E::Parse { .. } | E::InvalidPath(_) | E::InvalidName(_) | E::TooLarge { .. } => {
            http_err!(BAD_REQUEST, "{err}")
        }
        E::Io(e) => http_err!(INTERNAL_SERVER_ERROR, "I/O error: {e}"),
        E::Other(e) => http_err!(INTERNAL_SERVER_ERROR, "{e}"),
    }
}

/// Shorthand for `Err(to_http(err))`.
pub fn bail_http<T>(err: pve_meta_core::Error) -> Result<T, Error> {
    Err(to_http(err))
}
