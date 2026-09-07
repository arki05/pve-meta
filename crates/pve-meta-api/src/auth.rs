//! PVE authentication: local ticket verification, CSRF tokens and API-token parsing.
//!
//! `pve-metad` does not run its own login endpoint — users authenticate against the real
//! `pveproxy` and present the resulting `PVEAuthCookie` ticket (or a `PVEAPIToken` header) to
//! us. Tickets are RSA-SHA1 signatures (real PVE tickets, *not* the SHA-256 scheme used by
//! `proxmox_auth_api::Ticket::verify`), so we hand-roll parsing/verification here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Error, anyhow, bail};
use hmac::{Hmac, Mac};
use http::{HeaderMap, Method};
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Public};
use openssl::sign::Verifier;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

use proxmox_auth_api::types::{Authid, Userid};

const RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// Clock-skew tolerance: a ticket/csrf token may claim to be from up to 5 minutes in the future.
const GRACE_SECS: i64 = 300;
const DEFAULT_TICKET_LIFETIME: i64 = 7200;

/// Env var to override the ticket/csrf validity window (seconds). Default 7200 (2h, like PVE).
pub const TICKET_LIFETIME_ENV: &str = "PVE_META_TICKET_LIFETIME";

fn ticket_lifetime() -> i64 {
    std::env::var(TICKET_LIFETIME_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TICKET_LIFETIME)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

struct Loaded {
    keys: Vec<PKey<Public>>,
    key_mtimes: (Option<SystemTime>, Option<SystemTime>),
    csrf_secret: Vec<u8>,
    www_key_mtime: Option<SystemTime>,
}

struct State {
    last_check: Option<Instant>,
    loaded: Option<Loaded>,
}

static GLOBAL: std::sync::OnceLock<PveAuth> = std::sync::OnceLock::new();

/// Initializes the process-global [`PveAuth`] from the standard pmxcfs paths. Only `pve-metad`
/// calls this (the CLI runs unauthenticated, in-process); [`global`] returns `None` if it was
/// never called.
pub fn init_default() {
    let _ = GLOBAL.set(PveAuth::from_pmxcfs());
}

/// The process-global [`PveAuth`], if [`init_default`] was called.
pub fn global() -> Option<&'static PveAuth> {
    GLOBAL.get()
}

/// Holds the parsed PVE cluster keyring (`authkey.pub[.old]`) and the CSRF HMAC secret derived
/// from `pve-www.key`, reloading them lazily (at most once every 10s) when the source file
/// mtimes change.
pub struct PveAuth {
    authkey_pub: PathBuf,
    authkey_pub_old: PathBuf,
    www_key: PathBuf,
    state: Mutex<State>,
}

/// Why a caller's credentials were rejected. `Display` produces a message suitable for logging;
/// it deliberately never echoes back attacker-controlled ticket/token contents.
#[derive(Debug)]
pub enum AuthFailure {
    /// No credentials were presented at all.
    Missing,
    /// Credentials were presented but are invalid/expired/malformed.
    Invalid(String),
}

impl std::fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthFailure::Missing => write!(f, "no credentials provided"),
            AuthFailure::Invalid(msg) => write!(f, "invalid credentials: {msg}"),
        }
    }
}

impl std::error::Error for AuthFailure {}

impl PveAuth {
    pub fn new(
        authkey_pub: impl Into<PathBuf>,
        authkey_pub_old: impl Into<PathBuf>,
        www_key: impl Into<PathBuf>,
    ) -> Self {
        Self {
            authkey_pub: authkey_pub.into(),
            authkey_pub_old: authkey_pub_old.into(),
            www_key: www_key.into(),
            state: Mutex::new(State {
                last_check: None,
                loaded: None,
            }),
        }
    }

    /// Standard PVE cluster paths: `/etc/pve/authkey.pub[.old]`, `/etc/pve/pve-www.key`.
    pub fn from_pmxcfs() -> Self {
        Self::new(
            "/etc/pve/authkey.pub",
            "/etc/pve/authkey.pub.old",
            "/etc/pve/pve-www.key",
        )
    }

    fn load(&self) -> Result<Loaded, Error> {
        let current_pem = std::fs::read(&self.authkey_pub)
            .with_context(|| format!("reading {}", self.authkey_pub.display()))?;
        let mut keys = vec![
            PKey::public_key_from_pem(&current_pem).context("parsing authkey.pub")?,
        ];
        let old_mtime = file_mtime(&self.authkey_pub_old);
        if let Ok(old_pem) = std::fs::read(&self.authkey_pub_old) {
            if let Ok(k) = PKey::public_key_from_pem(&old_pem) {
                keys.push(k);
            }
        }
        let www_key_bytes = std::fs::read(&self.www_key)
            .with_context(|| format!("reading {}", self.www_key.display()))?;
        let csrf_secret = hmac_sha256_b64(b"", &www_key_bytes)?.into_bytes();

        Ok(Loaded {
            keys,
            key_mtimes: (file_mtime(&self.authkey_pub), old_mtime),
            csrf_secret,
            www_key_mtime: file_mtime(&self.www_key),
        })
    }

    /// (Re)loads the keyring/secret if this is the first use, if the recheck interval elapsed
    /// and a source file's mtime changed, or if nothing was ever successfully loaded.
    fn with_loaded<T>(&self, f: impl FnOnce(&Loaded) -> Result<T, Error>) -> Result<T, Error> {
        let mut state = self.state.lock().unwrap();
        let need_check = match state.last_check {
            None => true,
            Some(t) => t.elapsed() >= RECHECK_INTERVAL,
        };
        if need_check || state.loaded.is_none() {
            let changed = match &state.loaded {
                None => true,
                Some(l) => {
                    l.key_mtimes != (file_mtime(&self.authkey_pub), file_mtime(&self.authkey_pub_old))
                        || l.www_key_mtime != file_mtime(&self.www_key)
                }
            };
            if changed {
                match self.load() {
                    Ok(loaded) => state.loaded = Some(loaded),
                    Err(e) if state.loaded.is_some() => {
                        tracing::warn!("failed to reload PVE auth keys, keeping old ones: {e:#}");
                    }
                    Err(e) => return Err(e),
                }
            }
            state.last_check = Some(Instant::now());
        }
        let loaded = state
            .loaded
            .as_ref()
            .ok_or_else(|| anyhow!("PVE auth keys not loaded"))?;
        f(loaded)
    }

    /// The number of public keys currently loaded (for `/meta/health`'s `auth.keyring_keys`).
    /// Returns 0 if the keyring has never successfully loaded.
    pub fn key_count(&self) -> usize {
        self.with_loaded(|l| Ok(l.keys.len())).unwrap_or(0)
    }

    /// Verifies a PVE ticket (`PVE:<data>:<HEX8 time>::<base64 sig>`) against `now = now()`.
    pub fn verify_ticket(&self, ticket: &str) -> Result<Userid, AuthFailure> {
        self.verify_ticket_at(ticket, now_unix())
    }

    /// Same as [`verify_ticket`](Self::verify_ticket) but with an explicit "current time"
    /// (unix seconds), for deterministic tests.
    pub fn verify_ticket_at(&self, ticket: &str, now: i64) -> Result<Userid, AuthFailure> {
        let parsed = parse_ticket(ticket).map_err(|e| AuthFailure::Invalid(e.to_string()))?;
        if parsed.prefix != "PVE" {
            return Err(AuthFailure::Invalid("wrong ticket prefix".into()));
        }
        let age = now - parsed.time;
        let lifetime = ticket_lifetime();
        if !(age > -GRACE_SECS && age < lifetime) {
            return Err(AuthFailure::Invalid("ticket expired or not yet valid".into()));
        }

        let message = format!("PVE:{}:{:08X}", parsed.data, parsed.time);
        let ok = self
            .with_loaded(|loaded| {
                for key in &loaded.keys {
                    let mut verifier = Verifier::new(MessageDigest::sha1(), key)?;
                    verifier.update(message.as_bytes())?;
                    if verifier.verify(&parsed.signature)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })
            .map_err(|e| AuthFailure::Invalid(e.to_string()))?;

        if !ok {
            return Err(AuthFailure::Invalid("signature verification failed".into()));
        }

        let username = parsed.data.replace("%3A", ":");
        username
            .parse::<Userid>()
            .map_err(|e| AuthFailure::Invalid(format!("invalid userid in ticket: {e}")))
    }

    /// Builds a fresh `<HEX8 time>:<base64 hmac>` CSRF prevention token for `username`.
    pub fn assemble_csrf(&self, username: &str) -> Result<String, Error> {
        self.assemble_csrf_at(username, now_unix())
    }

    pub fn assemble_csrf_at(&self, username: &str, now: i64) -> Result<String, Error> {
        let time_hex = format!("{now:08X}");
        let msg = format!("{time_hex}:{username}");
        let secret = self.with_loaded(|l| Ok(l.csrf_secret.clone()))?;
        let sig = hmac_sha256_b64(&secret, msg.as_bytes())?;
        Ok(format!("{time_hex}:{sig}"))
    }

    /// Verifies a `CSRFPreventionToken` header value for `username`.
    pub fn verify_csrf(&self, username: &str, token: &str) -> bool {
        self.verify_csrf_at(username, token, now_unix())
    }

    pub fn verify_csrf_at(&self, username: &str, token: &str, now: i64) -> bool {
        let Some((time_hex, _sig)) = token.split_once(':') else {
            return false;
        };
        let Ok(time) = i64::from_str_radix(time_hex, 16) else {
            return false;
        };
        let age = now - time;
        let lifetime = ticket_lifetime();
        if !(age > -GRACE_SECS && age < lifetime) {
            return false;
        }
        match self.assemble_csrf_at(username, time) {
            Ok(expected) => constant_time_eq(expected.as_bytes(), token.as_bytes()),
            Err(_) => false,
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// HMAC-SHA256, base64-(no-pad)-encoded. Uses a pure-Rust implementation (not OpenSSL) because
/// the CSRF secret derivation requires an **empty** HMAC key (matching Perl's
/// `Digest::SHA::hmac_sha256_base64($data)` single-argument form, i.e. `key = ""`), which
/// OpenSSL's `PKey::hmac` does not accept.
fn hmac_sha256_b64(key: &[u8], data: &[u8]) -> Result<String, Error> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| anyhow!("invalid HMAC key: {e}"))?;
    mac.update(data);
    Ok(proxmox_base64::encode_no_pad(mac.finalize().into_bytes()))
}

struct ParsedTicket {
    prefix: String,
    data: String,
    time: i64,
    signature: Vec<u8>,
}

/// Parses the wire format `prefix:data:time::signature` (aad is always empty for our use).
fn parse_ticket(ticket: &str) -> Result<ParsedTicket, Error> {
    let parts: Vec<&str> = ticket.splitn(5, ':').collect();
    let [prefix, data, time_hex, _aad, sig_b64] = parts[..] else {
        bail!("malformed ticket (wrong number of fields)");
    };
    let time = i64::from_str_radix(time_hex, 16).context("invalid ticket timestamp")?;
    // Real PVE tickets are signed with Perl's MIME::Base64::encode_base64 (standard alphabet,
    // padded); accept either padding form.
    let signature = proxmox_base64::decode(sig_b64).context("invalid ticket signature encoding")?;
    Ok(ParsedTicket {
        prefix: prefix.to_string(),
        data: data.to_string(),
        time,
        signature,
    })
}

/// Result of successfully identifying a caller.
pub struct Identity {
    /// The string to record as `rpcenv.set_auth_id(...)` (a `Userid` or `Authid`).
    pub auth_id: String,
}

/// Parses `Authorization: PVEAPIToken=user@realm!tokenid=SECRET` (split on the *last* `=`,
/// matching real PVE — not `proxmox-auth-api`'s built-in `:`-splitting parser). The secret is
/// **not** verified in v1 (trusted lab); the token id becomes the auth id.
fn parse_token_header(headers: &HeaderMap) -> Option<Result<Identity, AuthFailure>> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    let rest = value
        .strip_prefix("PVEAPIToken=")
        .or_else(|| value.strip_prefix("PVEAPIToken "))?;
    Some(
        rest.rsplit_once('=')
            .ok_or_else(|| AuthFailure::Invalid("malformed PVEAPIToken header".into()))
            .and_then(|(tokenid, _secret)| {
                tokenid
                    .parse::<Authid>()
                    .map(|authid| Identity {
                        auth_id: authid.to_string(),
                    })
                    .map_err(|e| AuthFailure::Invalid(format!("invalid token id: {e}")))
            }),
    )
}

/// Extracts a PVE auth ticket from the `Cookie` header(s), preferring `__Host-PVEAuthCookie`.
pub fn extract_ticket_cookie(headers: &HeaderMap) -> Option<String> {
    let mut fallback = None;
    for cookie in headers.get_all(http::header::COOKIE).iter().filter_map(|c| c.to_str().ok()) {
        if let Some(t) = extract_cookie(cookie, "__Host-PVEAuthCookie") {
            return Some(t);
        }
        if fallback.is_none() {
            fallback = extract_cookie(cookie, "PVEAuthCookie");
        }
    }
    fallback
}

/// Extracts the CSRF prevention token header, if present.
pub fn extract_csrf_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("CSRFPreventionToken")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn extract_cookie(cookie: &str, name: &str) -> Option<String> {
    for pair in cookie.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(name) {
            if let Some(value) = value.strip_prefix('=') {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Identifies the caller of a request: tries the `PVEAPIToken` header first, then a
/// `PVEAuthCookie`/`__Host-PVEAuthCookie` ticket. Non-`GET` cookie-authenticated requests must
/// additionally carry a valid `CSRFPreventionToken` header.
pub fn identify(auth: &PveAuth, headers: &HeaderMap, method: &Method) -> Result<Identity, AuthFailure> {
    if let Some(result) = parse_token_header(headers) {
        return result;
    }

    let ticket = extract_ticket_cookie(headers).ok_or(AuthFailure::Missing)?;
    let userid = auth.verify_ticket(&ticket)?;

    if method != Method::GET {
        let token = extract_csrf_header(headers)
            .ok_or_else(|| AuthFailure::Invalid("missing CSRFPreventionToken header".into()))?;
        if !auth.verify_csrf(&userid.to_string(), &token) {
            return Err(AuthFailure::Invalid("invalid CSRFPreventionToken".into()));
        }
    }

    Ok(Identity {
        auth_id: userid.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_TICKET: &str = "PVE:metatest@pve:6A9EF47D::YP61VNCHyWmBaghc/PGb94IVfUZ/d6fQPlYYauZkBlaYUYgF8IajcoTZSDWAXVdHyY7hVDNhWVtG2k7k3kUC/Ni4uCUS63JW6Q2B/hoGv6Ccepv9tNt8spJ5YE2/pX+o/EtuGnrdVX2bU+Hyfctwp2UH3dn+3Q+xexNhmNRYbVIvQolXILrXQXeKvu/IsqhG6xYf8+YvqZEyem1Bqf4CfxqOyE971jX1e7KzK4I+WfvjAyM/4bNT89fpt7BF0yn0fip5qXStu24O/vlkGllCaW9wsP1KpnmWUrNOGU4ngVQCzoAEG3LSworQWz+VlVvr4b+9ZfWXyBfdWLK07euD/A==";

    fn fixture_auth() -> PveAuth {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        PveAuth::new(
            format!("{dir}/authkey.pub"),
            format!("{dir}/authkey.pub.does-not-exist"),
            format!("{dir}/pve-www.key"),
        )
    }

    #[test]
    fn ticket_fixture_verifies_at_signing_time() {
        let auth = fixture_auth();
        let now = 0x6A9EF47Du32 as i64;
        let userid = auth
            .verify_ticket_at(FIXTURE_TICKET, now)
            .expect("fixture ticket must verify");
        assert_eq!(userid.to_string(), "metatest@pve");
    }

    #[test]
    fn ticket_fixture_rejected_when_expired() {
        let auth = fixture_auth();
        let now = 0x6A9EF47Du32 as i64 + DEFAULT_TICKET_LIFETIME + 10;
        assert!(auth.verify_ticket_at(FIXTURE_TICKET, now).is_err());
    }

    #[test]
    fn ticket_fixture_rejected_with_tampered_signature() {
        let auth = fixture_auth();
        let now = 0x6A9EF47Du32 as i64;
        let mut tampered = FIXTURE_TICKET.to_string();
        tampered.pop();
        tampered.push('X');
        assert!(auth.verify_ticket_at(&tampered, now).is_err());
    }

    #[test]
    fn ticket_bad_prefix_rejected() {
        let auth = fixture_auth();
        let bad = FIXTURE_TICKET.replacen("PVE:", "PDM:", 1);
        assert!(auth.verify_ticket_at(&bad, 0x6A9EF47D).is_err());
    }

    #[test]
    fn csrf_round_trip() {
        let auth = fixture_auth();
        let now = 1_700_000_000i64;
        let token = auth.assemble_csrf_at("root@pam", now).unwrap();
        assert!(auth.verify_csrf_at("root@pam", &token, now));
        assert!(auth.verify_csrf_at("root@pam", &token, now + 100));
        // wrong user
        assert!(!auth.verify_csrf_at("someone@pve", &token, now));
        // expired
        assert!(!auth.verify_csrf_at("root@pam", &token, now + DEFAULT_TICKET_LIFETIME + 10));
        // tampered
        let mut bad = token.clone();
        bad.push('x');
        assert!(!auth.verify_csrf_at("root@pam", &bad, now));
    }

    #[test]
    fn token_header_parses_last_equals_split() {
        // Real PVE API token secrets are opaque UUIDs (no embedded '='), but the format is
        // `user@realm!tokenid=SECRET` — splitting on the *last* '=' is still what real PVE does
        // (see docs/DAEMON-SPEC.md), so verify that specifically rather than assuming secrets
        // never contain '='.
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "PVEAPIToken=root@pam!mytoken=8b1c1234-5678-90ab-cdef-1234567890ab"
                .parse()
                .unwrap(),
        );
        let identity = parse_token_header(&headers).unwrap().unwrap();
        assert_eq!(identity.auth_id, "root@pam!mytoken");
    }

    #[test]
    fn cookie_extraction_prefers_host_prefixed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            "PVEAuthCookie=plain; __Host-PVEAuthCookie=preferred"
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_ticket_cookie(&headers).as_deref(), Some("preferred"));
    }
}
