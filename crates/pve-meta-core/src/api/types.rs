//! Wire types: request/response shapes that cross the Perl/Rust boundary
//! (`docs/DESIGN.md` §8). Everything here carries data and decides nothing —
//! no authorization, no lint, no I/O. If a type would ever need a method
//! with a branch in it, it belongs in `api.rs`, not here.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{NodeName, Origin, PrefixDef, RegistryFailure};

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
    /// The guest's current node, from the vmlist: whose prefix files join the
    /// packaged and cluster ones for this guest (`docs/DESIGN.md` §3). Absent
    /// for a registry document, and then no node's files apply. A name that is
    /// not a node name does not deserialize.
    #[serde(default)]
    pub node: Option<NodeName>,
}

/// `GET /meta/version`: one unscoped token over the whole store
/// (`docs/DESIGN.md` §6).
#[derive(Debug, Clone, Serialize)]
pub struct ApiVersion {
    /// A content hash over the store; poll it.
    pub token: String,
}

/// One row of `GET /meta/guests` (`docs/DESIGN.md` §4, §8): only for a guest
/// the caller has `VM.Audit` on -- one without it is omitted entirely, so
/// every field here is unconditional.
#[derive(Debug, Clone, Serialize)]
pub struct GuestListEntry {
    pub vmid: u32,
    pub node: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub name: Option<String>,
    pub tags: Vec<String>,
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

/// `GET /meta/access`: the caller's access to one document (`docs/DESIGN.md`
/// §4) -- exactly `acl.read`/`acl.write`, computed by Perl from PVE's ACLs.
#[derive(Debug, Clone, Serialize)]
pub struct ApiAccess {
    pub read: bool,
    pub write: bool,
}

/// One row of `GET /meta/guests`' input: the vmlist row Perl already has,
/// plus that guest's `VM.Audit` answer and tags. Perl owns the vmlist and the
/// guest properties — there is exactly one reader of `/etc/pve/.vmlist` per
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
    /// `VM.Audit` on this guest (`docs/DESIGN.md` §4): a listing includes a
    /// guest exactly when this is set.
    #[serde(default)]
    pub read: bool,
}

/// `GET /meta/prefixes`' element: a loaded prefix, or a file that failed to
/// load. Untagged, so each element serializes as itself -- a `PrefixDef`'s own
/// shape, or [`FailedPrefix`]'s -- and a consumer that only wants the good
/// ones can filter on whether `error` is present rather than unwrap a variant
/// tag that has no counterpart in the file format.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PrefixEntry {
    Loaded(PrefixDef),
    Failed(FailedPrefix),
}

/// A prefix file that did not load, keyed like a loaded [`PrefixDef`]
/// (`prefix`, not `name`) so one array can mix both and a reader can find
/// either by the same field. No filesystem path: `origin` already says which
/// file it is, which is what a repair needs (`docs/DESIGN.md` §1).
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
