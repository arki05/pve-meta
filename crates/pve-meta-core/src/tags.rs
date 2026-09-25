//! A guest's PVE tags as a list: one splitting rule for everything that reads
//! them, since a prefix's selector matches against what this returns
//! (`docs/DESIGN.md` §3).

/// `raw` split into tags: the cluster stores them `;`-separated, and `,` and
/// whitespace separate too, so a hand-written list still parses. Empty runs
/// drop out. `PVE::API2::Ext::Meta::parse_tags`, where the tag string comes
/// out of the guest's properties, calls this through `PVE::RS::Meta`.
pub fn split_tags(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ';' || c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_split_on_semicolons_commas_and_whitespace() {
        for (raw, expected) in [
            ("", vec![]),
            (";;", vec![]),
            ("traefik", vec!["traefik"]),
            ("a;b;;c", vec!["a", "b", "c"]),
            ("a, b c\ttraefik\n", vec!["a", "b", "c", "traefik"]),
        ] {
            assert_eq!(split_tags(raw), expected, "{raw:?}");
        }
    }
}
