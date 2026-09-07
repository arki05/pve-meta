//! `/meta/registry` and `/meta/schemas/{vmid}`: a convenience view of `datacenter.operators`.

use anyhow::Error;
use serde_json::{Map, Value};

use proxmox_router::{Permission, Router};
use proxmox_schema::api;

use pve_meta_core::model;
use pve_meta_core::store::DocId;

use crate::error::to_http;
use crate::store::store;

/// Reads `datacenter.operators` (comment keys stripped), or an empty object if the datacenter
/// document does not exist.
pub fn read_operators() -> Result<Map<String, Value>, Error> {
    let value = match store().read(DocId::Datacenter) {
        Ok(doc) => {
            let mut v = doc.value;
            model::strip_comments(&mut v);
            v
        }
        Err(pve_meta_core::Error::NotFound(_)) => Value::Object(Map::new()),
        Err(e) => return Err(to_http(e)),
    };
    Ok(value
        .get("operators")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default())
}

/// Builds the `[{ name, claims, schemas, description? }, ...]` list from `operators`, in
/// document order.
pub fn registry_entries(operators: &Map<String, Value>) -> Vec<Value> {
    operators
        .iter()
        .map(|(name, op)| {
            let mut entry = serde_json::json!({
                "name": name,
                "claims": op.get("claims").cloned().unwrap_or(Value::Array(vec![])),
                "schemas": op.get("schemas").cloned().unwrap_or(Value::Object(Map::new())),
            });
            if let Some(desc) = op.get("description") {
                entry["description"] = desc.clone();
            }
            entry
        })
        .collect()
}

#[api(access: { permission: &Permission::Anybody })]
/// Lists the registered operators from the datacenter document's `operators` section.
pub fn registry() -> Result<Value, Error> {
    let operators = read_operators()?;
    Ok(Value::Array(registry_entries(&operators)))
}

const VMID_SCHEMA: proxmox_schema::Schema = proxmox_schema::IntegerSchema::new("The guest's vmid.")
    .minimum(100)
    .maximum(999_999_999)
    .schema();

#[api(
    input: { properties: { vmid: { schema: VMID_SCHEMA } } },
    access: { permission: &Permission::Anybody },
)]
/// The JSON schemas applicable to a guest's document (i.e. for namespaces it actually uses),
/// keyed by namespace prefix.
pub fn schemas_for_guest(vmid: u32) -> Result<Value, Error> {
    let doc = store().read(DocId::Guest(vmid)).map_err(to_http)?;
    let mut value = doc.value;
    model::strip_comments(&mut value);
    let namespaces: std::collections::HashSet<String> = model::namespaces(&value).into_iter().collect();

    let operators = read_operators()?;
    let mut out = Map::new();
    for op in operators.values() {
        let Some(claims) = op.get("claims").and_then(|c| c.as_array()) else {
            continue;
        };
        let Some(schemas) = op.get("schemas").and_then(|s| s.as_object()) else {
            continue;
        };
        for claim in claims {
            let Some(prefix) = claim.get("prefix").and_then(|p| p.as_str()) else {
                continue;
            };
            if namespaces.contains(prefix) {
                if let Some(schema) = schemas.get(prefix) {
                    out.insert(prefix.to_string(), schema.clone());
                }
            }
        }
    }
    Ok(Value::Object(out))
}

pub const REGISTRY_ROUTER: Router = Router::new().get(&API_METHOD_REGISTRY);
pub const SCHEMAS_ROUTER: Router = Router::new().match_all("vmid", &Router::new().get(&API_METHOD_SCHEMAS_FOR_GUEST));
