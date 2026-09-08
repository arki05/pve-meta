//! Operator registrations and the grammar they declare (`GET /meta/operators`).
//!
//! One registration names a principal (`authid`) and the prefixes it may read or write
//! (`docs/DESIGN.md` §3). Each scope carries a **selector** — `all`, or `tag: <t>` — that
//! decides which guests it applies to, and optionally a **grammar**, a `PVE::JSONSchema`
//! object describing the subtree under the prefix.
//!
//! The tree uses registrations for two things, and neither is an access decision (that is
//! `GET /meta/access`, `crate::model::Access`):
//!
//! * **Rows that ought to exist.** A grammar's `properties` name keys the operator expects;
//!   the tree shows them greyed with their default even when the document has never carried
//!   them, and the declared `description` is the Description column's tooltip.
//! * **Access.** Every registration whose scope covers a row is listed in the Access column
//!   — `rw` by name, `ro` muted — with the selector that made it apply to this guest.
//!
//! Pure module: unit-tested natively (`cargo test --lib`).

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::{Mode, flag_value};

/// One entry of `GET /meta/operators`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Operator {
    /// The registration file's name, e.g. `traefik`.
    #[serde(default)]
    pub name: String,
    /// The PVE user or token id the registration is for.
    #[serde(default)]
    pub authid: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub scopes: Vec<OperatorScope>,
}

/// One scope of a registration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OperatorScope {
    /// Dotted key path, any depth.
    pub prefix: String,
    /// What the operator may do there. Unknown spellings are not a grant, so they parse
    /// as `ro` — this is display-only anyway (the caller's own rights come from
    /// `/meta/access`).
    #[serde(default = "read_only")]
    pub mode: Mode,
    /// Which guests the scope applies to.
    #[serde(default)]
    pub selector: Selector,
    /// `PVE::JSONSchema` description of the subtree at `prefix`, if the operator declared
    /// one. Kept as a raw `Value`: the dialect is Perl's, not serde's, and the tree only
    /// ever reads a handful of well-known keys out of it (see the `schema_*` helpers).
    #[serde(default)]
    pub grammar: Option<Value>,
}

fn read_only() -> Mode {
    Mode::Ro
}

/// Which guests a scope applies to (`docs/DESIGN.md` §3).
///
/// Deliberately not an enum: an unknown selector shape (a future `pool:`, a typo) must
/// deserialize successfully and then match *nothing*, rather than fail the whole
/// `/meta/operators` response for every other operator in it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Selector {
    #[serde(default)]
    pub all: Option<Value>,
    #[serde(default)]
    pub tag: Option<String>,
}

impl Selector {
    /// True if this selector picks a guest carrying `tags`.
    pub fn matches(&self, tags: &[String]) -> bool {
        if self.all.as_ref().is_some_and(flag_value) {
            return true;
        }
        match &self.tag {
            Some(tag) => tags.iter().any(|t| t == tag),
            None => false,
        }
    }

    /// A short human label for the Access column's tooltip: `all`, `tag: traefik`, or
    /// nothing.
    pub fn label(&self) -> Option<String> {
        if self.all.as_ref().is_some_and(flag_value) {
            return Some("all".to_string());
        }
        self.tag.as_ref().map(|tag| format!("tag: {tag}"))
    }
}

/// The `properties` map of a `PVE::JSONSchema` object schema.
pub fn schema_properties(schema: &Value) -> Option<&Map<String, Value>> {
    schema.get("properties")?.as_object()
}

/// The schema of one property of an object schema.
pub fn schema_property<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema_properties(schema)?.get(key)
}

/// Walk `schema` down a relative dotted path through `properties`.
pub fn schema_at<'a>(schema: &'a Value, path: &str) -> Option<&'a Value> {
    let mut node = schema;
    if path.is_empty() {
        return Some(node);
    }
    for segment in path.split('.') {
        node = schema_property(node, segment)?;
    }
    Some(node)
}

/// `description` of a schema node.
pub fn schema_description(schema: &Value) -> Option<String> {
    schema.get("description")?.as_str().map(str::to_string)
}

/// `default` of a schema node.
pub fn schema_default(schema: &Value) -> Option<&Value> {
    schema.get("default")
}

/// `type` of a schema node (`string`, `integer`, `number`, `boolean`, `object`, `array`).
pub fn schema_type(schema: &Value) -> Option<&str> {
    schema.get("type")?.as_str()
}

/// `enum` of a schema node, as display strings.
pub fn schema_enum(schema: &Value) -> Option<Vec<String>> {
    let list = schema.get("enum")?.as_array()?;
    Some(list.iter().map(scalar_to_string).collect())
}

/// `minimum` of a schema node — the number editor's lower bound.
pub fn schema_minimum(schema: &Value) -> Option<f64> {
    schema.get("minimum")?.as_f64()
}

/// `maximum` of a schema node — the number editor's upper bound.
pub fn schema_maximum(schema: &Value) -> Option<f64> {
    schema.get("maximum")?.as_f64()
}

/// `format` of a schema node: a `PVE::JSONSchema` format name.
pub fn schema_format(schema: &Value) -> Option<&str> {
    schema.get("format")?.as_str()
}

/// Checks `value` against the `PVE::JSONSchema` format named `format`.
///
/// **An unrecognised format constrains nothing.** `PVE::JSONSchema` registers dozens of
/// formats and this is a client-side affordance, not the authority — the server's one
/// lint is (`docs/DESIGN.md` §4). A checker we are not sure of would reject input that is
/// actually valid, which is strictly worse than not checking: the operator that owns the
/// key is the one that ultimately validates it.
///
/// The set below is exactly the set `ui-extjs` maps onto proxmoxlib's own vtypes, so the
/// two implementations accept and reject the same strings. Adding a format means adding
/// it in both places.
pub fn check_format(format: &str, value: &str) -> Result<(), String> {
    let ok = match format {
        "ipv4" => is_ipv4(value),
        "ipv6" => is_ipv6(value),
        "ip" => is_ipv4(value) || is_ipv6(value),
        "CIDRv4" => is_cidr(value, true),
        "CIDRv6" => is_cidr(value, false),
        "CIDR" => is_cidr(value, true) || is_cidr(value, false),
        "mac-addr" => is_mac(value),
        "dns-name" => is_dns_name(value),
        "address" => is_dns_name(value) || is_ipv4(value) || is_ipv6(value),
        "email" => is_email(value),
        _ => return Ok(()),
    };
    match ok {
        true => Ok(()),
        false => Err(format!("not a valid {format}")),
    }
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                // No leading zeros: "01" is not how an address is written, and some
                // resolvers read it as octal.
                && (p.len() == 1 || !p.starts_with('0'))
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

/// A deliberately permissive IPv6 check: group count and shape, one `::` at most, and an
/// embedded IPv4 tail (`::ffff:192.0.2.1`) accepted — rejecting that would be the exact
/// false negative this module must not produce.
fn is_ipv6(s: &str) -> bool {
    if s.matches("::").count() > 1 {
        return false;
    }
    // Counts the 16-bit groups in one half, or `None` if any is malformed. A trailing
    // IPv4 literal stands for two groups.
    let groups = |part: &str| -> Option<usize> {
        if part.is_empty() {
            return Some(0);
        }
        let mut n = 0;
        let fields: Vec<&str> = part.split(':').collect();
        for (i, g) in fields.iter().enumerate() {
            if i + 1 == fields.len() && g.contains('.') {
                if !is_ipv4(g) {
                    return None;
                }
                n += 2;
                continue;
            }
            if g.is_empty() || g.len() > 4 || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            n += 1;
        }
        Some(n)
    };
    match s.split_once("::") {
        // Compressed: the two halves together must leave room for at least one zero group.
        Some((head, tail)) => match (groups(head), groups(tail)) {
            (Some(a), Some(b)) => a + b <= 7,
            _ => false,
        },
        None => groups(s) == Some(8),
    }
}

fn is_cidr(s: &str, v4: bool) -> bool {
    let Some((addr, len)) = s.split_once('/') else {
        return false;
    };
    let (addr_ok, max) = match v4 {
        true => (is_ipv4(addr), 32),
        false => (is_ipv6(addr), 128),
    };
    addr_ok
        && !len.is_empty()
        && len.bytes().all(|b| b.is_ascii_digit())
        && len.parse::<u16>().is_ok_and(|n| n <= max)
}

fn is_mac(s: &str) -> bool {
    let sep = match s.contains('-') {
        true => '-',
        false => ':',
    };
    let parts: Vec<&str> = s.split(sep).collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_dns_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn is_email(s: &str) -> bool {
    match s.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !local.bytes().any(|b| b.is_ascii_whitespace() || b == b'@')
                && is_dns_name(domain)
        }
        None => false,
    }
}

/// The key order a grammar declares for an object schema, if any.
///
/// `data` is unordered on the wire and the tree sorts alphabetically (`docs/DESIGN.md`
/// §4/§8); an object schema may override that for its own properties with an `order`
/// array of key names. Keys it lists come first, in that order; everything else follows
/// alphabetically. Keys in `order` that the schema does not declare are ignored.
pub fn schema_order(schema: &Value) -> Vec<String> {
    match schema.get("order").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    }
}

/// A scalar as the string a form field shows.
pub fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operators() -> Vec<Operator> {
        serde_json::from_value(json!([{
            "name": "traefik",
            "authid": "svc@pve!traefik",
            "description": "Traefik dynamic-configuration provider",
            "scopes": [{
                "prefix": "traefik",
                "mode": "rw",
                "selector": { "tag": "traefik" },
                "grammar": {
                    "type": "object",
                    "properties": {
                        "spec": {
                            "type": "object",
                            "order": ["host", "port"],
                            "properties": {
                                "host": { "type": "string", "description": "Public host name" },
                                "port": {
                                    "type": "integer", "minimum": 1, "maximum": 65535,
                                    "optional": 1, "default": 80,
                                },
                            },
                        },
                    },
                },
            }],
        }]))
        .unwrap()
    }

    #[test]
    fn a_registration_parses_whole() {
        let ops = operators();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].authid, "svc@pve!traefik");
        assert_eq!(ops[0].scopes[0].mode, Mode::Rw);
        assert_eq!(ops[0].scopes[0].selector.tag.as_deref(), Some("traefik"));
    }

    #[test]
    fn a_selector_matches_by_tag_or_by_all() {
        let all: Selector = serde_json::from_value(json!({"all": true})).unwrap();
        assert!(all.matches(&[]));
        assert_eq!(all.label().as_deref(), Some("all"));

        // The wire may spell a Perl boolean as 1.
        let perl_all: Selector = serde_json::from_value(json!({"all": 1})).unwrap();
        assert!(perl_all.matches(&[]));

        let tagged: Selector = serde_json::from_value(json!({"tag": "traefik"})).unwrap();
        assert!(tagged.matches(&["traefik".to_string()]));
        assert!(!tagged.matches(&["netbird".to_string()]));
        assert_eq!(tagged.label().as_deref(), Some("tag: traefik"));
    }

    #[test]
    fn an_unknown_selector_parses_and_grants_nothing() {
        // A future `{ pool: name }` must not fail the whole response.
        let future: Selector = serde_json::from_value(json!({"pool": "prod"})).unwrap();
        assert!(!future.matches(&["prod".to_string()]));
        assert_eq!(future.label(), None);
        // `all: 0` is not "all".
        let off: Selector = serde_json::from_value(json!({"all": 0})).unwrap();
        assert!(!off.matches(&[]));
    }

    #[test]
    fn schema_navigation_walks_properties() {
        let ops = operators();
        let grammar = ops[0].scopes[0].grammar.clone().unwrap();

        let host = schema_at(&grammar, "spec.host").unwrap();
        assert_eq!(schema_type(host), Some("string"));
        assert_eq!(
            schema_description(host).as_deref(),
            Some("Public host name"),
        );

        let port = schema_at(&grammar, "spec.port").unwrap();
        assert_eq!(schema_default(port), Some(&json!(80)));
        assert_eq!(
            schema_order(schema_at(&grammar, "spec").unwrap()),
            ["host", "port"]
        );

        assert!(schema_at(&grammar, "spec.missing").is_none());
        assert!(schema_at(&grammar, "").is_some());
    }

    #[test]
    fn schema_range_and_format_are_read_from_the_node() {
        let schema = json!({"type": "integer", "minimum": 1, "maximum": 65535});
        assert_eq!(schema_minimum(&schema), Some(1.0));
        assert_eq!(schema_maximum(&schema), Some(65535.0));
        assert_eq!(schema_format(&schema), None);
        assert_eq!(
            schema_format(&json!({"type": "string", "format": "ipv4"})),
            Some("ipv4")
        );
        assert_eq!(schema_minimum(&json!({"type": "string"})), None);
    }

    #[test]
    fn an_unrecognised_format_constrains_nothing() {
        // The important direction: a format this UI does not know must never reject
        // input the operator considers valid.
        assert!(check_format("pve-storage-id", "anything at all").is_ok());
        assert!(check_format("", "").is_ok());
    }

    #[test]
    fn ipv4_and_cidr_formats() {
        for good in ["0.0.0.0", "192.0.2.1", "255.255.255.255"] {
            assert!(check_format("ipv4", good).is_ok(), "{good}");
        }
        for bad in ["1.2.3", "1.2.3.4.5", "256.0.0.1", "01.2.3.4", "1.2.3.a", ""] {
            assert!(check_format("ipv4", bad).is_err(), "{bad}");
        }
        assert!(check_format("CIDRv4", "192.0.2.0/24").is_ok());
        assert!(check_format("CIDRv4", "192.0.2.0/33").is_err());
        assert!(check_format("CIDRv4", "192.0.2.0").is_err());
        assert!(check_format("CIDR", "192.0.2.0/24").is_ok());
        assert!(check_format("CIDR", "2001:db8::/32").is_ok());
    }

    #[test]
    fn ipv6_accepts_compression_and_an_embedded_ipv4() {
        for good in [
            "::",
            "::1",
            "2001:db8::1",
            "::ffff:192.0.2.1",
            "2001:0db8:0000:0000:0000:0000:0000:0001",
        ] {
            assert!(check_format("ipv6", good).is_ok(), "{good}");
        }
        for bad in ["2001:db8", "2001::db8::1", "gggg::1", "1.2.3.4", "12345::1"] {
            assert!(check_format("ipv6", bad).is_err(), "{bad}");
        }
        // `ip` takes either family.
        assert!(check_format("ip", "192.0.2.1").is_ok());
        assert!(check_format("ip", "2001:db8::1").is_ok());
        assert!(check_format("ip", "not-an-address").is_err());
    }

    #[test]
    fn mac_dns_address_and_email() {
        assert!(check_format("mac-addr", "aa:bb:cc:dd:ee:ff").is_ok());
        assert!(check_format("mac-addr", "AA-BB-CC-DD-EE-FF").is_ok());
        assert!(check_format("mac-addr", "aa:bb:cc:dd:ee").is_err());
        assert!(check_format("dns-name", "wiki.example.com").is_ok());
        assert!(check_format("dns-name", "-bad.example.com").is_err());
        assert!(check_format("dns-name", "a..b").is_err());
        // `address` is either a name or an address.
        assert!(check_format("address", "wiki.example.com").is_ok());
        assert!(check_format("address", "192.0.2.1").is_ok());
        assert!(check_format("address", "not a host").is_err());
        assert!(check_format("email", "ops@example.com").is_ok());
        assert!(check_format("email", "ops-at-example.com").is_err());
    }

    #[test]
    fn enum_values_render_as_strings() {
        let schema = json!({"type": "string", "enum": ["http", "https", 8080]});
        assert_eq!(
            schema_enum(&schema).unwrap(),
            ["http".to_string(), "https".to_string(), "8080".to_string()],
        );
        assert!(schema_enum(&json!({"type": "string"})).is_none());
    }
}
