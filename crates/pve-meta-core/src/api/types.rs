//! Wire types: request/response shapes crossing the Perl/Rust boundary (`docs/DESIGN.md` §6); no authorization, no lint, no I/O -- a type needing a branch belongs in `api.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{NodeName, Origin, PrefixDef, RegistryFailure};

/// The caller, as `PVE::API2::Ext::Meta` computes it (`docs/DESIGN.md` §4): authid, the two ACL answers, and a guest's tags/node for selector resolution.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallerAcl {
    /// The caller's PVE authid (`user@realm`, optionally `!tokenid`).
    #[serde(default)]
    pub authid: String,
    /// `VM.Audit` on `/vms/<vmid>`; always true for a registry document.
    #[serde(default)]
    pub read: bool,
    /// `VM.Config.Options` on `/vms/<vmid>`; `Sys.Modify` on `/` for a registry document.
    #[serde(default)]
    pub write: bool,
    /// The guest's PVE tags. Empty for a registry document.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The guest's current node from the vmlist, joining packaged/cluster prefix files for it (`docs/DESIGN.md` §3); absent for a registry document.
    #[serde(default)]
    pub node: Option<NodeName>,
}

/// `GET /meta/version`: one unscoped content-hash token over the whole store (`docs/DESIGN.md` §6).
#[derive(Debug, Clone, Serialize)]
pub struct ApiVersion {
    pub token: String,
}

/// One row of `GET /meta/guests` (`docs/DESIGN.md` §4, §6): only for a guest the caller has `VM.Audit` on.
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

/// A read's result: exactly one of `data` (`format=json`) or `text` (`format=yaml`) is populated.
#[derive(Debug, Clone, Serialize)]
pub struct ApiViewDocument {
    pub id: String,
    pub view: String,
    pub digest: String,
    /// Present when `format=json` (`docs/DESIGN.md` §5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Present when `format=yaml`: the file's own text, a canonical dump, or -- with `parse_error` -- the raw text of a document that does not parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Present only when the stored document is not valid YAML (`docs/DESIGN.md` §5): the parser's message.
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

/// `GET /meta/access`: the caller's access to one document (`docs/DESIGN.md` §4).
#[derive(Debug, Clone, Serialize)]
pub struct ApiAccess {
    pub read: bool,
    pub write: bool,
}

/// One row of `GET /meta/guests`' input: the vmlist row Perl already has, plus that guest's `VM.Audit` answer and tags.
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
    /// `VM.Audit` on this guest: a listing includes a guest exactly when this is set.
    #[serde(default)]
    pub read: bool,
}

/// `GET /meta/prefixes`' element: a loaded [`PrefixDef`], or a [`FailedPrefix`] that did not load.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PrefixEntry {
    Loaded(PrefixDef),
    Failed(FailedPrefix),
}

/// A prefix file that did not load, keyed like a loaded [`PrefixDef`] (`prefix`, not `name`).
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
