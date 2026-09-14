//! The notes block: how a guest's document travels through a vzdump backup
//! (`docs/DESIGN.md` §9, `docs/LIFECYCLE.md`).
//!
//! A backup carries the guest config, and the config's notes carry arbitrary
//! text. So at backup time the document is appended to the *archive's copy*
//! of the notes as one marked block, and at restore time the block is read
//! back into the store and stripped from the notes again. The live config
//! never carries it. A host without pve-meta restores the block as ordinary
//! notes text, readable and harmless.
//!
//! The block, as it appears in the notes:
//!
//! `````text
//! [pve-meta v1 vmid=105 time=2026-09-14T03:00:12Z sha256=9f2c…]
//! ````yaml
//! traefik:
//!   spec: { host: web.example, port: 8080 }
//! ````
//! [/pve-meta]
//! `````
//!
//! * The two bracket lines delimit the block; [`find`] looks for them, never
//!   for the fence. They render as plain text on a stock host's Summary.
//! * The fence is for markdown rendering only. Four backticks, so a YAML
//!   block scalar containing a three-backtick line cannot close it early; a
//!   plain YAML scalar can never begin with a backtick at all.
//! * The header carries provenance for humans (`vmid`, `time`) and the
//!   document's own digest (`sha256`, the store's digest of the file text).
//!   Restoring to a different vmid is normal, so `vmid` never blocks an
//!   import; the digest is checked and a mismatch is reported, not refused.
//!
//! Plain YAML, verbatim: no base64, no compression. PVE's own text encoder
//! percent-escapes every byte outside printable ASCII (plus colon and
//! percent) per notes line and its decoder restores them, so the document
//! round-trips through both guest config formats byte for byte. One size cap
//! ([`MAX_YAML_BYTES`]) and one form; a document over the cap is not carried
//! and the backup log says so.
//!
//! One quirk of PVE's format is guarded against here: `#` is not escaped, so
//! a notes line whose text begins with `qmdump#` or `vzdump#` is written as
//! `#qmdump#…`, which vzdump silently drops. A document cannot produce such a
//! line (every top-level line starts with a key from the charset, nested
//! lines are indented, the marker and fence lines start with a bracket or a
//! backtick), and [`render`] refuses rather than emit one if it ever did.
//!
//! Two more facts about the transit, checked against both guest parsers:
//! a notes line keeps its trailing whitespace (only the end of the whole
//! notes text is trimmed, and the block's last line is its end line), and
//! a document line that is itself one of the block's delimiters is refused
//! by [`render`], so a block scalar can never end the block early.

use serde::Serialize;

use crate::digest;
use crate::error::{Error, Result};
use crate::store::{DocId, MetaStore};

/// The block format version written by [`render`] and accepted by [`find`].
pub const VERSION: u32 = 1;

/// The largest document, in bytes of YAML, that is carried in a backup. Above
/// this, [`export`] refuses and the backup runs without it (the vzdump log
/// records why). Well below pmxcfs's own file limit and small enough that a
/// stock host's notes stay usable.
pub const MAX_YAML_BYTES: usize = 16 * 1024;

const BEGIN_PREFIX: &str = "[pve-meta v";
const END_LINE: &str = "[/pve-meta]";
const FENCE_OPEN: &str = "````yaml";
const FENCE_CLOSE: &str = "````";

/// Notes-line prefixes that vzdump's `assemble` strips from a config copy
/// and restore drops again (`#qmdump#…`, `#vzdump#…`), with the leading `#`
/// that the notes encoder adds removed.
const DROPPED_PREFIXES: [&str; 2] = ["qmdump#", "vzdump#"];

/// What a block's header line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The block format version (`v1`).
    pub version: u32,
    /// The vmid the document belonged to when the backup was taken.
    pub vmid: Option<u32>,
    /// When the backup was assembled, RFC 3339 UTC.
    pub time: Option<String>,
    /// The store's digest of the document text at that time.
    pub sha256: Option<String>,
}

/// A block found in a notes text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Byte range of the block within the notes: from the start of the
    /// header line to the end of the end line (its newline excluded).
    pub start: usize,
    pub end: usize,
    pub header: Header,
    /// The document text between the fences, without a trailing newline.
    pub yaml: String,
}

/// How [`import`] treats a block when the store already has a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// A restore: the block is the backup being restored, so it wins.
    Restore,
    /// The install-time scan: a document that is already there was put
    /// there by a node that already runs pve-meta, so it wins and the block
    /// is only stripped.
    Install,
}

impl ImportMode {
    /// Parses the Perl-facing spelling.
    pub fn parse(s: &str) -> Result<ImportMode> {
        match s {
            "restore" => Ok(ImportMode::Restore),
            "install" => Ok(ImportMode::Install),
            other => Err(Error::InvalidName(format!(
                "unknown import mode '{other}' (expected 'restore' or 'install')"
            ))),
        }
    }
}

/// What [`import`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportAction {
    /// The notes carry no block; nothing to do.
    None,
    /// The document was written from the block and the block stripped.
    Imported,
    /// A document was already there and kept; the block was stripped.
    Stripped,
}

/// The result of [`import`]: what happened, and the notes with the block
/// removed when one was. `description` is `None` when the notes are to be
/// left as they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Import {
    pub action: ImportAction,
    pub description: Option<String>,
}

/// Renders the block for `yaml` (a document's file text) with `digest` (the
/// store's digest of that text) and `now` (unix seconds).
///
/// # Errors
/// [`Error::TooLarge`] above [`MAX_YAML_BYTES`]; [`Error::InvalidName`] if a
/// line of the block would begin with a prefix vzdump drops (see the module
/// docs), which a document cannot produce.
pub fn render(vmid: u32, yaml: &str, digest: &str, now: u64) -> Result<String> {
    let yaml = yaml.trim_end_matches('\n');
    if yaml.len() > MAX_YAML_BYTES {
        return Err(Error::TooLarge {
            size: yaml.len() as u64,
            max: MAX_YAML_BYTES as u64,
        });
    }
    let block = format!(
        "{BEGIN_PREFIX}{VERSION} vmid={vmid} time={} sha256={digest}]\n{FENCE_OPEN}\n{yaml}\n{FENCE_CLOSE}\n{END_LINE}",
        rfc3339_utc(now)
    );
    for line in block.lines() {
        if DROPPED_PREFIXES.iter().any(|p| line.starts_with(p)) {
            return Err(Error::InvalidName(format!(
                "a notes line may not begin with '{}': {line:?}",
                &line[..7]
            )));
        }
    }
    // The block's own delimiters may not occur inside it either: a block
    // scalar holding a line that is exactly the end line would end the block
    // early, and a header line inside it would be a second header. Neither
    // is likely from a document; both are made impossible here.
    for line in yaml.lines() {
        let l = line.trim_end();
        if l == END_LINE || l.starts_with(BEGIN_PREFIX) {
            return Err(Error::InvalidName(format!(
                "a document line may not be a block delimiter: {line:?}"
            )));
        }
    }
    Ok(block)
}

/// Finds the block in a notes text, if there is one. A block is a header
/// line with an end line somewhere after it; the first such header wins.
/// A header line with no end line after it is not a block at all — someone
/// wrote `[pve-meta v1]` in their notes — and is never reported, since the
/// report would repeat on every config write of that guest. A block whose
/// fences are not where the format puts them is malformed and reported.
///
/// # Errors
/// [`Error::Parse`] for a malformed block.
pub fn find(description: &str) -> Result<Option<Found>> {
    let Some(start) = line_start_of(description, |l| {
        l.starts_with(BEGIN_PREFIX) && l.ends_with(']')
    }) else {
        return Ok(None);
    };
    let rest = &description[start..];
    if !rest.split('\n').any(|l| l.trim_end() == END_LINE) {
        return Ok(None);
    }
    let malformed = |msg: &str| Error::Parse {
        format: crate::format::Format::Yaml,
        msg: format!("malformed pve-meta notes block: {msg}"),
        at: None,
    };
    let mut lines = rest.split('\n');
    let header_line = lines.next().unwrap_or_default();
    let header = parse_header(header_line).ok_or_else(|| malformed("unreadable header line"))?;
    if header.version != VERSION {
        return Err(malformed(&format!(
            "unsupported version v{}",
            header.version
        )));
    }
    if lines.next().map(str::trim_end) != Some(FENCE_OPEN) {
        return Err(malformed("the header is not followed by the opening fence"));
    }
    let mut body: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim_end() == END_LINE {
            closed = true;
            break;
        }
        body.push(line);
    }
    if !closed {
        return Err(malformed("no end line"));
    }
    if body.last().map(|l| l.trim_end()) != Some(FENCE_CLOSE) {
        return Err(malformed(
            "the end line is not preceded by the closing fence",
        ));
    }
    body.pop();
    let yaml = body.join("\n");
    // The end offset: the header, the fence, every body line, the closing
    // fence and the end line, joined by newlines.
    let consumed_lines = 2 + body.len() + 2;
    let end = start
        + rest
            .split_inclusive('\n')
            .take(consumed_lines)
            .map(str::len)
            .sum::<usize>();
    let end = if description[..end].ends_with('\n') {
        end - 1
    } else {
        end
    };
    Ok(Some(Found {
        start,
        end,
        header,
        yaml,
    }))
}

/// The notes with `found`'s block removed: the block itself, the newline
/// after it, and the blank lines that separated it from the text before.
/// Trailing whitespace is trimmed, so a notes text that was only the block
/// comes back empty.
pub fn strip(description: &str, found: &Found) -> String {
    let before = description[..found.start].trim_end();
    let after = description[found.end..].trim_start_matches('\n');
    let mut out = String::with_capacity(before.len() + after.len() + 1);
    out.push_str(before);
    if !before.is_empty() && !after.trim().is_empty() {
        out.push('\n');
    }
    out.push_str(after);
    out.trim_end().to_string()
}

/// The block for `vmid`'s current document, or `None` if the guest has no
/// document. What the vzdump `assemble` hooks append to the archive's copy
/// of the notes.
///
/// # Errors
/// [`Error::TooLarge`] for a document above [`MAX_YAML_BYTES`];
/// [`Error::Parse`] for a document that does not parse (a backup carries a
/// document the restore can write, or nothing, and says which); I/O errors.
pub fn export(store: &MetaStore, vmid: u32, now: u64) -> Result<Option<String>> {
    let doc = match store.read(&DocId::Guest(vmid)) {
        Ok(doc) => doc,
        Err(Error::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    if let Some(msg) = doc.parse_error {
        return Err(Error::Parse {
            format: crate::format::Format::Yaml,
            msg: format!("the stored document does not parse: {msg}"),
            at: None,
        });
    }
    render(vmid, &doc.raw, &doc.digest, now).map(Some)
}

/// Reads the block out of `description` into `vmid`'s document and strips
/// it, per `mode`. Called from the patched `write_config` on every restore
/// (`ImportMode::Restore`) and from `pve-meta scan-notes` at install
/// (`ImportMode::Install`). The caller holds the document's cluster lock.
///
/// A digest mismatch between the header and the block's text is reported
/// with [`crate::warn`] and the text is imported anyway: someone edited it
/// in the notes on purpose. Nothing here fails a restore; an error leaves
/// the notes untouched, block included, for the caller to warn about.
///
/// # Errors
/// [`Error::Parse`] for a malformed block or YAML the store refuses;
/// [`Error::Lint`] for a document that fails the lint; I/O errors.
pub fn import(store: &MetaStore, vmid: u32, description: &str, mode: ImportMode) -> Result<Import> {
    let Some(found) = find(description)? else {
        return Ok(Import {
            action: ImportAction::None,
            description: None,
        });
    };
    let doc_id = DocId::Guest(vmid);
    let stripped = strip(description, &found);
    let existing = match mode {
        ImportMode::Restore => false,
        ImportMode::Install => store.digest_of(&doc_id)?.is_some(),
    };
    if existing {
        crate::audit(&format!(
            "backup notes block on {doc_id} stripped: a document is already there"
        ));
        return Ok(Import {
            action: ImportAction::Stripped,
            description: Some(stripped),
        });
    }
    let text = format!("{}\n", found.yaml);
    if let Some(expected) = &found.header.sha256 {
        let actual = digest::digest(text.as_bytes());
        if *expected != actual {
            crate::warn_line!(
                "backup notes block on {doc_id}: digest {} does not match its text ({}); importing the text as it is",
                &expected[..12.min(expected.len())],
                &actual[..12]
            );
        }
    }
    let written = store.put_raw(&doc_id, &text, None)?;
    crate::audit(&format!(
        "backup notes block imported into {doc_id} (from vmid {}, taken {}): digest {}",
        found
            .header
            .vmid
            .map_or_else(|| "?".to_string(), |v| v.to_string()),
        found.header.time.as_deref().unwrap_or("?"),
        &written.document.digest[..12],
    ));
    Ok(Import {
        action: ImportAction::Imported,
        description: Some(stripped),
    })
}

/// Byte offset of the start of the first line for which `pred` holds.
fn line_start_of(text: &str, pred: impl Fn(&str) -> bool) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if pred(line.trim_end_matches(['\n', '\r'])) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// `[pve-meta v1 vmid=105 time=… sha256=…]` → [`Header`]. Fields other than
/// the version are optional and unordered; unknown fields are ignored.
fn parse_header(line: &str) -> Option<Header> {
    let inner = line
        .trim_end()
        .strip_prefix(BEGIN_PREFIX)?
        .strip_suffix(']')?;
    let mut fields = inner.split_whitespace();
    let version: u32 = fields.next()?.parse().ok()?;
    let mut header = Header {
        version,
        vmid: None,
        time: None,
        sha256: None,
    };
    for field in fields {
        let Some((k, v)) = field.split_once('=') else {
            continue;
        };
        match k {
            "vmid" => header.vmid = v.parse().ok(),
            "time" => header.time = Some(v.to_string()),
            "sha256" => header.sha256 = Some(v.to_string()),
            _ => {}
        }
    }
    Some(header)
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SSZ`, without a date crate. The civil
/// date is Howard Hinnant's `civil_from_days`.
fn rfc3339_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MetaStore;

    const YAML: &str =
        "traefik:\n  spec: { host: web.example, port: 8080 }\nbackup:\n  retention: 7\n";

    fn digest_of(yaml: &str) -> String {
        digest::digest(yaml.as_bytes())
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_789_331_412), "2026-09-13T20:30:12Z");
    }

    #[test]
    fn render_then_find_round_trips_the_document() {
        let block = render(105, YAML, &digest_of(YAML), 1_789_331_412).unwrap();
        assert!(block.starts_with("[pve-meta v1 vmid=105 time=2026-09-13T20:30:12Z sha256="));
        assert!(block.ends_with("\n````\n[/pve-meta]"));
        let found = find(&block).unwrap().unwrap();
        assert_eq!(found.start, 0);
        assert_eq!(found.end, block.len());
        assert_eq!(found.yaml, YAML.trim_end());
        assert_eq!(found.header.vmid, Some(105));
        assert_eq!(
            found.header.sha256.as_deref(),
            Some(digest_of(YAML).as_str())
        );
        assert_eq!(strip(&block, &found), "");
    }

    #[test]
    fn block_after_user_notes_is_found_and_stripped_cleanly() {
        let block = render(105, YAML, &digest_of(YAML), 0).unwrap();
        let notes = format!("# web01\n\nowner: arki\n\n{block}");
        let found = find(&notes).unwrap().unwrap();
        assert_eq!(&notes[found.start..found.end], block);
        assert_eq!(strip(&notes, &found), "# web01\n\nowner: arki");

        let notes = format!("before\n\n{block}\n\nafter");
        let found = find(&notes).unwrap().unwrap();
        assert_eq!(strip(&notes, &found), "before\nafter");
    }

    #[test]
    fn no_block_is_none_and_a_mention_is_not_a_block() {
        assert_eq!(find("").unwrap(), None);
        assert_eq!(find("see [pve-meta v1] for details").unwrap(), None);
        // A header with no end line after it is a mention, not a block, and
        // must never warn: it would warn on every config write.
        assert_eq!(find("[pve-meta v1 vmid=1]\nnot a fence").unwrap(), None);
        assert_eq!(find("[pve-meta v1]\n````yaml\na: 1\n````\n").unwrap(), None);
        // With an end line, the shape between is checked.
        assert!(find("[pve-meta v1 vmid=1]\nnot a fence\n[/pve-meta]")
            .unwrap_err()
            .to_string()
            .contains("opening fence"));
        assert!(find("[pve-meta v2 vmid=1]\n````yaml\n````\n[/pve-meta]")
            .unwrap_err()
            .to_string()
            .contains("v2"));
    }

    #[test]
    fn render_refuses_a_document_line_that_is_a_delimiter() {
        // The guard mirrors `find`: a delimiter is a whole, unindented line.
        // A document cannot produce one (its top level is a map, and a
        // block scalar's lines are indented), so these are not documents,
        // just the text that would break the block if it ever were.
        assert!(matches!(
            render(1, "[/pve-meta]\n", "", 0),
            Err(Error::InvalidName(_))
        ));
        assert!(matches!(
            render(1, "a: 1\n[pve-meta v1 vmid=9]\n", "", 0),
            Err(Error::InvalidName(_))
        ));
        // Indented, a delimiter is neither found by `find` nor refused here.
        let indented = "note: |\n  [/pve-meta]\n  [pve-meta v1 vmid=9]\n";
        let block = render(1, indented, "", 0).unwrap();
        assert_eq!(find(&block).unwrap().unwrap().yaml, indented.trim_end());
        let inline = "note: '[pve-meta v1] is the marker'\n";
        assert!(render(1, inline, "", 0).is_ok());
    }

    #[test]
    fn a_three_backtick_line_inside_the_document_does_not_close_the_block() {
        let yaml = "readme: |\n  ```sh\n  echo hi\n  ```\n";
        let block = render(7, yaml, &digest_of(yaml), 0).unwrap();
        let found = find(&block).unwrap().unwrap();
        assert_eq!(found.yaml, yaml.trim_end());
    }

    #[test]
    fn render_refuses_the_size_cap_and_a_dropped_prefix() {
        let big = format!("k: {}\n", "x".repeat(MAX_YAML_BYTES));
        assert!(matches!(
            render(1, &big, "", 0),
            Err(Error::TooLarge { .. })
        ));
        // Not producible by a document (a key cannot contain '#'), but the
        // guard is what makes that a fact rather than a hope.
        let bad = "note: |\nqmdump#map:x\n";
        assert!(matches!(render(1, bad, "", 0), Err(Error::InvalidName(_))));
    }

    #[test]
    fn no_rendered_line_carries_a_dropped_prefix_for_a_real_document() {
        let yaml = "a:\n  note: |\n    qmdump#not-at-line-start\nvzdump: fine\n";
        let block = render(1, yaml, &digest_of(yaml), 0).unwrap();
        for line in block.lines() {
            assert!(
                !DROPPED_PREFIXES.iter().any(|p| line.starts_with(p)),
                "{line}"
            );
        }
    }

    #[test]
    fn export_import_restore_writes_the_document_and_strips_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        assert_eq!(export(&store, 105, 0).unwrap(), None);

        store.put_raw(&DocId::Guest(105), YAML, None).unwrap();
        let block = export(&store, 105, 0).unwrap().unwrap();
        let notes = format!("hello\n\n{block}");

        // Restore to another vmid, over an existing document: the block wins.
        store
            .put_raw(&DocId::Guest(200), "old: true\n", None)
            .unwrap();
        let res = import(&store, 200, &notes, ImportMode::Restore).unwrap();
        assert_eq!(res.action, ImportAction::Imported);
        assert_eq!(res.description.as_deref(), Some("hello"));
        assert_eq!(store.read(&DocId::Guest(200)).unwrap().raw, YAML);

        // Nothing to do on plain notes.
        let res = import(&store, 200, "hello", ImportMode::Restore).unwrap();
        assert_eq!(res.action, ImportAction::None);
        assert_eq!(res.description, None);
    }

    #[test]
    fn import_install_keeps_an_existing_document_and_strips() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        let block = render(105, YAML, &digest_of(YAML), 0).unwrap();

        store
            .put_raw(&DocId::Guest(300), "kept: true\n", None)
            .unwrap();
        let res = import(&store, 300, &block, ImportMode::Install).unwrap();
        assert_eq!(res.action, ImportAction::Stripped);
        assert_eq!(res.description.as_deref(), Some(""));
        assert_eq!(store.read(&DocId::Guest(300)).unwrap().raw, "kept: true\n");

        let res = import(&store, 301, &block, ImportMode::Install).unwrap();
        assert_eq!(res.action, ImportAction::Imported);
        assert_eq!(store.read(&DocId::Guest(301)).unwrap().raw, YAML);
    }

    #[test]
    fn import_refuses_text_the_store_would_refuse_and_leaves_the_notes() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        let block = "[pve-meta v1 vmid=1 sha256=0]\n````yaml\n- not: a map\n````\n[/pve-meta]";
        assert!(import(&store, 400, block, ImportMode::Restore).is_err());
        assert!(matches!(
            store.read(&DocId::Guest(400)),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn export_refuses_a_document_that_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("500.yaml"), "a: [unclosed\n").unwrap();
        let store = MetaStore::new(dir.path());
        assert!(matches!(export(&store, 500, 0), Err(Error::Parse { .. })));
    }
}
