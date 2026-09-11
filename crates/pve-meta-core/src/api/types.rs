//! Wire types: request/response shapes that cross the Perl/Rust boundary
//! (`docs/DESIGN.md` §8). Everything here carries data and decides nothing —
//! no authorization, no lint, no I/O. If a type would ever need a method
//! with a branch in it, it belongs in `api.rs`, not here.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{Origin, Permission, PrefixDef, RegistryFailure};
use crate::scopes::Scope;

/// The caller, as `PVE::API2::Ext::Meta` computes it: their authid, the two
/// ACL answers for the document being addressed, and (for a guest) that
/// guest's PVE tags, which resolve the registrations' selectors.
///
/// This is a native Perl hash on the wire; `read`/`write` arrive as ordinary
/// Perl scalars and are converted by truthiness.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallerAcl {
    /// The caller's PVE authid (`user@realm`, optionally `!tokenid`).
    #[serde(default)]
    pub authid: String,
    /// `VM.Audit` on `/vms/<vmid>`; always true for a registry document, which
    /// every authenticated user may read.
    #[serde(default)]
    pub read: bool,
    /// `VM.Config.Options` on `/vms/<vmid>`; `Sys.Modify` on `/` for a
    /// registry document.
    #[serde(default)]
    pub write: bool,
    /// The guest's PVE tags. Empty for a registry document.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `GET /meta/version`.
#[derive(Debug, Clone, Serialize)]
pub struct ApiVersion {
    /// A content hash over the store; poll it.
    pub token: String,
    /// The newest document mtime, as a unix timestamp.
    pub changed: u64,
    /// With `detail`: every document's own digest, sorted by id, so a caller
    /// that saw the token move can tell **which** documents to re-read instead
    /// of re-listing the store. With an `id` as well, just that one document.
    ///
    /// Unfiltered by design. A digest is not sensitive (`docs/DESIGN.md` §1
    /// puts digests and listings out of scope), and filtering would cost a
    /// grant computation per document on the one endpoint whose whole purpose
    /// is to be cheap enough to poll.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<ApiDocumentDigest>>,
}

/// One row of `GET /meta/version?detail=1`.
#[derive(Debug, Clone, Serialize)]
pub struct ApiDocumentDigest {
    /// A vmid, `prefixes/<name>` or `permissions/<name>`.
    pub id: String,
    pub digest: String,
}

/// One row of `GET /meta/guests` (`docs/DESIGN.md` §8).
#[derive(Debug, Clone, Serialize)]
pub struct GuestListEntry {
    pub vmid: u32,
    /// Only with `VM.Audit`.
    pub node: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Only with `VM.Audit`.
    pub name: Option<String>,
    /// Only with `VM.Audit`.
    pub tags: Option<Vec<String>>,
    /// `""` when the guest has no document.
    pub digest: String,
}

/// One entry of a write's `touched` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiTouched {
    pub path: String,
    pub op: String,
}

/// A read's result: exactly one of `data` (`format=json`) or `text`
/// (`format=yaml`) is populated.
#[derive(Debug, Clone, Serialize)]
pub struct ApiViewDocument {
    pub id: String,
    pub view: String,
    pub digest: String,
    /// Present when `format=json`. A native structure, unordered once it is a
    /// Perl hash; key order is not a wire contract (`docs/DESIGN.md` §7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Present when `format=yaml`: the file's own text for the root view, a
    /// canonical dump for a sub-view — or, alongside `parse_error`, the raw
    /// text of a document that does not parse, so it can be repaired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Present only when the stored document is not valid YAML
    /// (`docs/DESIGN.md` §7): the parser's message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

/// A write's result.
#[derive(Debug, Clone, Serialize)]
pub struct ApiPutResult {
    pub id: String,
    pub view: String,
    pub digest: String,
    pub touched: Vec<ApiTouched>,
}

/// `GET /meta/access`: the caller's grants for one document, with selectors
/// already resolved.
#[derive(Debug, Clone, Serialize)]
pub struct ApiAccess {
    pub read: bool,
    pub write: bool,
    pub scopes: Vec<Scope>,
    /// The guest's PVE tags — empty for any other document.
    ///
    /// Here so an editor can resolve `selector: {tag: …}` without a second
    /// request: the alternative was `GET /meta/guests`, which reads, parses
    /// and digests **every** document in the cluster to answer a question
    /// about one guest. The tags come from the same place either way — Perl
    /// computed them for this request's ACL check — so the rule that the
    /// server supplies tags and the client only matches them is unchanged.
    ///
    /// Filtered exactly as `GET /meta/guests` filters the same field: a
    /// caller without `VM.Audit` on the guest gets an empty list, not the
    /// tags (`docs/DESIGN.md` §8).
    pub tags: Vec<String>,
}

/// One row of `GET /meta/guests`' input: the vmlist row Perl already has,
/// plus that guest's ACL answers and tags. Perl owns the vmlist and the guest
/// properties — there is exactly one reader of `/etc/pve/.vmlist` per
/// request, and guest config parsing is not re-implemented here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GuestInput {
    pub vmid: u32,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `VM.Audit` on this guest.
    #[serde(default)]
    pub read: bool,
    /// `VM.Config.Options` on this guest. Never consulted by a listing; it is
    /// here so one row shape serves every caller.
    #[serde(default)]
    pub write: bool,
}

/// `GET /meta/prefixes`' element: a loaded prefix, or a file that failed to
/// load. Untagged, so each element serializes as itself -- a `PrefixDef`'s own
/// shape, or [`FailedPrefix`]'s -- and a consumer that only wants the good
/// ones can filter on whether `error` is present rather than unwrap a variant
/// tag that has no counterpart in the file format.
///
/// Two concrete enums (this and [`PermissionEntry`]) rather than one generic
/// `RegistryEntry<T>`: a failed prefix has to serialize with the key `prefix`
/// and a failed permission with `name`, because that is the key their loaded
/// counterparts already use, and a bare `RegistryFailure` (whose field is
/// always `name`) cannot supply both from one `Serialize` impl.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PrefixEntry {
    Loaded(PrefixDef),
    Failed(FailedPrefix),
}

/// A prefix file that did not load, keyed like a loaded [`PrefixDef`]
/// (`prefix`, not `name`) so one array can mix both and a reader can find
/// either by the same field. No filesystem path: `origin` already says
/// packaged or cluster, which is what a repair needs (`docs/DESIGN.md` §1).
#[derive(Debug, Clone, Serialize)]
pub struct FailedPrefix {
    pub prefix: String,
    pub origin: Origin,
    pub error: String,
}

impl From<RegistryFailure> for FailedPrefix {
    fn from(f: RegistryFailure) -> Self {
        FailedPrefix { prefix: f.name, origin: f.origin, error: f.error }
    }
}

/// `GET /meta/permissions`' element: a loaded permission, or a file that
/// failed to load. See [`PrefixEntry`] for why this is untagged and why it is
/// its own enum rather than a shared generic.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PermissionEntry {
    Loaded(Permission),
    Failed(FailedPermission),
}

/// A permission file that did not load, keyed like a loaded [`Permission`]
/// (`name`). No filesystem path, for the same reason as [`FailedPrefix`].
#[derive(Debug, Clone, Serialize)]
pub struct FailedPermission {
    pub name: String,
    pub origin: Origin,
    pub error: String,
}

impl From<RegistryFailure> for FailedPermission {
    fn from(f: RegistryFailure) -> Self {
        FailedPermission { name: f.name, origin: f.origin, error: f.error }
    }
}
