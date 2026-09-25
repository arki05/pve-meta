//! Content digests used for optimistic concurrency and change detection.

use sha2::{Digest as _, Sha256};

/// Returns the lowercase hex-encoded SHA-256 digest of `bytes`.
pub fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hex::encode(hash)
}

/// The first twelve characters of `d`, the length a log line shows a digest
/// at. By characters, not bytes: a digest read back out of notes text (a
/// backup block's header) is whatever someone typed there, and a byte slice
/// through a multi-byte character would panic.
pub fn short(d: &str) -> &str {
    d.char_indices().nth(12).map_or(d, |(i, _)| &d[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_of_empty_is_known_sha256() {
        assert_eq!(
            digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn digest_is_lowercase_hex_and_deterministic() {
        let a = digest(b"hello world");
        let b = digest(b"hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn short_is_twelve_characters_or_all_of_a_shorter_one() {
        assert_eq!(short(&digest(b"")), "e3b0c44298fc");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
        assert_eq!(short("abcdefghijk\u{e9}xyz"), "abcdefghijk\u{e9}");
    }

    #[test]
    fn digest_changes_with_content() {
        assert_ne!(digest(b"a"), digest(b"b"));
    }
}
