//! The decision table of `docs/GUEST-FILES.md`: what a guest's document asks
//! for, what the manifest says was written, and what is in the guest now, in;
//! the file operations and the manifest they leave, out. Pure: nothing here
//! touches a guest.
//!
//! **Unsafe.** Every existing path above a target has to be a real directory,
//! not a symlink or a file, and the target a regular file or nothing;
//! otherwise the target is refused. A write lands where the document names it
//! or not at all, and nothing but a regular file is ever replaced or removed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::entry::{sources_conflict, GuestPath, LocalEdits, Owner, GuestFiles, Resolved};
use crate::manifest::{self, Manifest, Record};
use crate::GUEST_ROOT;

/// One path inside the guest, as `lstat` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Missing,
    Symlink,
    Dir,
    File {
        uid: u32,
        gid: u32,
        mode: u32,
        sha256: String,
    },
    /// A socket, a device, a fifo.
    Other,
}

/// What a target path holds, judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Missing,
    Present { sha256: String, mode: u32, owner: Owner },
    Unsafe(String),
}

/// What one sync does with one path, or would do. Its `Display` is the one
/// wording of the state, for `status` and the log alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    InSync,
    Create,
    Update,
    /// A local edit replaced: `overwrite`, or `sync --force`.
    Overwrite,
    KeptLocalEdit,
    Delete,
    /// Removed from the document and not in the guest either.
    Forget,
    /// Removed from the document, but the file is not what was written.
    Left(String),
    Refused(String),
}

impl Action {
    /// `true` when this changes a file.
    pub fn writes(&self) -> bool {
        matches!(self, Action::Create | Action::Update | Action::Overwrite | Action::Delete)
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::InSync => f.write_str("in sync"),
            Action::Create => f.write_str("create"),
            Action::Update => f.write_str("update"),
            Action::Overwrite => f.write_str("overwrite local edit"),
            Action::KeptLocalEdit => f.write_str("local edit kept"),
            Action::Delete => f.write_str("delete"),
            Action::Forget => f.write_str("forget (already gone)"),
            Action::Left(why) => write!(f, "left in place, no longer managed ({why})"),
            Action::Refused(why) => write!(f, "refused ({why})"),
        }
    }
}

/// One row of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub entry: String,
    /// `None` for an entry refused before it had a path.
    pub path: Option<GuestPath>,
    pub action: Action,
}

/// What a replace requires of the file at the moment it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    Missing,
    Sha256(String),
    /// The manifest, which only the daemon writes.
    Any,
}

/// A file operation inside the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// `content` written to `path`'s temp sibling with its mode and owner, then
    /// renamed over `path` if the file there is still what `expect` says.
    Place { path: String, content: Vec<u8>, mode: u32, owner: Owner, expect: Expect },
    /// `path` removed if it still hashes to `sha256`.
    Remove { path: String, sha256: String },
}

impl Op {
    pub fn path(&self) -> &str {
        match self {
            Op::Place { path, .. } | Op::Remove { path, .. } => path,
        }
    }
}

/// A sync, decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<Item>,
    /// The file operations, one per item that writes, in item order.
    pub ops: Vec<Op>,
    /// Directories to create, parent first.
    pub dirs: Vec<String>,
    /// The manifest once every operation has been done.
    pub manifest: Manifest,
}

impl Plan {
    /// `true` when a sync would change anything in the guest.
    pub fn changes(&self, old: &Manifest) -> bool {
        !self.ops.is_empty() || self.manifest != *old
    }

    /// The manifest after a commit: [`Plan::manifest`], with the path of every
    /// operation that was not done as it was in `old`.
    pub fn manifest_after(&self, old: &Manifest, done: &[bool]) -> Manifest {
        let mut m = self.manifest.clone();
        for (op, _) in self.ops.iter().zip(done).filter(|(_, d)| !**d) {
            let path = GuestPath::absolute(op.path()).expect("a planned path");
            match old.records.get(&path) {
                Some(r) => m.records.insert(path, r.clone()),
                None => m.records.remove(&path),
            };
        }
        m
    }
}

/// Every path whose node a plan needs: each file wanted or recorded, every
/// directory above it, and the manifest's directories.
pub fn probe_paths(files: &GuestFiles, manifest: &Manifest) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = manifest::directories().into_iter().collect();
    let mut add = |p: &GuestPath| {
        out.extend(p.directories());
        out.insert(p.to_string());
    };
    if let GuestFiles::Entries(entries) = files {
        for r in entries {
            if let Resolved::File(d) = r {
                add(&d.entry.path);
            }
        }
    }
    if let GuestFiles::Managed(items) = files {
        for d in items {
            add(&d.entry.path);
        }
    }
    manifest.records.keys().for_each(add);
    out
}

fn dir_problem(dir: &str, node: &Node) -> Option<String> {
    match node {
        Node::Missing => None,
        Node::Symlink => Some(format!("{dir} is a symlink")),
        Node::Dir => None,
        Node::File { .. } | Node::Other => Some(format!("{dir} is not a directory")),
    }
}

/// Judges one target from the nodes of it and its directories. A node that
/// was not probed counts as missing.
pub fn assess(path: &GuestPath, nodes: &BTreeMap<String, Node>) -> FileState {
    let node = |p: &str| nodes.get(p).cloned().unwrap_or(Node::Missing);
    for dir in path.directories() {
        let n = node(&dir);
        if let Some(why) = dir_problem(&dir, &n) {
            return FileState::Unsafe(why);
        }
        if n == Node::Missing {
            return FileState::Missing;
        }
    }
    match node(path.as_str()) {
        Node::Missing => FileState::Missing,
        Node::Symlink => FileState::Unsafe(format!("{path} is a symlink")),
        Node::Dir | Node::Other => {
            FileState::Unsafe(format!("{path} is not a regular file"))
        }
        Node::File { uid, gid, mode, sha256 } => {
            FileState::Present { sha256, mode, owner: Owner { uid, gid } }
        }
    }
}

/// Decides one sync. `force` overwrites local edits of wanted files; it never
/// deletes one.
pub fn plan(
    files: &GuestFiles,
    old: &Manifest,
    nodes: &BTreeMap<String, Node>,
    force: bool,
) -> Plan {
    let mut p = Plan {
        items: Vec::new(),
        ops: Vec::new(),
        dirs: Vec::new(),
        manifest: Manifest::default(),
    };

    // The manifest has to be writable for anything to be written.
    let root_problem = manifest::directories()
        .iter()
        .find_map(|d| dir_problem(d, nodes.get(d).unwrap_or(&Node::Missing)));
    let hold = match files {
        GuestFiles::Held(why) => Some(why.clone()),
        _ => root_problem,
    };
    if let Some(why) = hold {
        for r in old.records.values() {
            let action = Action::Refused(why.clone());
            p.items.push(Item { entry: r.entry.clone(), path: Some(r.path.clone()), action });
        }
        p.manifest = old.clone();
        return p;
    }
    let owned: Vec<Resolved>;
    let entries: &[Resolved] = match files {
        GuestFiles::Entries(entries) => entries.as_slice(),
        // Managed files arrive resolved; map them to the same shape. The
        // clone is per sync call, never the daemon's hot poll.
        GuestFiles::Managed(items) => {
            owned = items.iter().cloned().map(Resolved::File).collect();
            &owned
        }
        _ => &[][..],
    };

    let held: BTreeSet<&str> = entries
        .iter()
        .filter(|r| matches!(r, Resolved::Refused { .. }))
        .map(Resolved::name)
        .collect();
    let mut wanted: BTreeSet<&GuestPath> = BTreeSet::new();
    let mut dirs: BTreeSet<String> = BTreeSet::new();

    for r in entries {
        let d = match r {
            Resolved::File(d) => d,
            Resolved::Absent { .. } => continue,
            Resolved::Refused { name, reason } => {
                let action = Action::Refused(reason.clone());
                p.items.push(Item { entry: name.clone(), path: None, action });
                continue;
            }
        };
        let e = &d.entry;
        wanted.insert(&e.path);
        let record = old.records.get(&e.path);
        // Two writers, one path: the manifest's source wins and the claimant
        // is refused by name. `user/` entries among themselves keep the old
        // behaviour (a rename keeps its path).
        if let Some(r) = record {
            if sources_conflict(&e.source, &r.source) {
                let reason = format!(
                    "path '{}' is recorded for source '{}', not '{}'",
                    e.path, r.source, e.source
                );
                p.items.push(Item {
                    entry: e.name.clone(),
                    path: Some(e.path.clone()),
                    action: Action::Refused(reason),
                });
                p.manifest.records.insert(e.path.clone(), r.clone());
                continue;
            }
        }
        let (action, expect) = match assess(&e.path, nodes) {
            FileState::Unsafe(why) => (Action::Refused(why), None),
            FileState::Missing => (Action::Create, Some(Expect::Missing)),
            FileState::Present { sha256, mode, owner } => {
                if sha256 == d.sha256 && mode == e.mode && owner == e.owner {
                    (Action::InSync, None)
                } else if record.is_some_and(|r| r.sha256 == sha256) {
                    (Action::Update, Some(Expect::Sha256(sha256)))
                } else if e.local_edits == LocalEdits::Overwrite || force {
                    (Action::Overwrite, Some(Expect::Sha256(sha256)))
                } else {
                    (Action::KeptLocalEdit, None)
                }
            }
        };
        match action {
            Action::InSync | Action::Create | Action::Update | Action::Overwrite => {
                let r = Record {
                    entry: e.name.clone(),
                    path: e.path.clone(),
                    sha256: d.sha256.clone(),
                    source: e.source.clone(),
                };
                p.manifest.records.insert(e.path.clone(), r);
            }
            _ => {
                if let Some(r) = record {
                    p.manifest.records.insert(e.path.clone(), r.clone());
                }
            }
        }
        if action == Action::Create {
            let missing = e.path.directories().into_iter();
            dirs.extend(missing.filter(|d| !matches!(nodes.get(d), Some(Node::Dir))));
        }
        if let Some(expect) = expect {
            let path = e.path.to_string();
            p.ops.push(Op::Place {
                path,
                content: d.content.clone(),
                mode: e.mode,
                owner: e.owner,
                expect,
            });
        }
        p.items.push(Item { entry: e.name.clone(), path: Some(e.path.clone()), action });
    }

    for (path, r) in &old.records {
        if wanted.contains(path) {
            continue;
        }
        if held.contains(r.entry.as_str()) {
            p.manifest.records.insert(path.clone(), r.clone());
            continue;
        }
        let action = match assess(path, nodes) {
            FileState::Missing => Action::Forget,
            FileState::Present { sha256, .. } if sha256 == r.sha256 => {
                p.ops.push(Op::Remove { path: path.to_string(), sha256 });
                Action::Delete
            }
            FileState::Present { .. } => Action::Left("changed since it was written".into()),
            FileState::Unsafe(why) => Action::Left(why),
        };
        p.items.push(Item { entry: r.entry.clone(), path: Some(path.clone()), action });
    }

    if p.changes(old) && !matches!(nodes.get(GUEST_ROOT), Some(Node::Dir)) {
        dirs.insert(GUEST_ROOT.to_string());
    }
    p.dirs = dirs.into_iter().collect();
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{user_source, Desired, Entry, FileFormat};
    use pve_meta_core::digest::digest;
    use pve_meta_core::path::Path;

    fn root_dirs() -> BTreeMap<String, Node> {
        let dir = Node::Dir;
        ["/etc", "/etc/pve-meta", "/etc/app"]
            .into_iter()
            .map(|d| (d.to_string(), dir.clone()))
            .collect()
    }

    fn wants(path: &str, content: &str, edits: LocalEdits) -> GuestFiles {
        GuestFiles::Entries(vec![Resolved::File(Desired {
            entry: Entry {
                name: "t".into(),
                view: Some(Path::parse("a").unwrap()),
                path: GuestPath::parse(path).unwrap(),
                format: FileFormat::Raw,
                mode: 0o444,
                owner: Owner::ROOT,
                local_edits: edits,
                source: user_source("t"),
            },
            content: content.as_bytes().to_vec(),
            sha256: digest(content.as_bytes()),
        })])
    }

    fn file(content: &str, mode: u32) -> Node {
        Node::File { uid: 0, gid: 0, mode, sha256: digest(content.as_bytes()) }
    }

    fn recorded(path: &str, content: &str) -> Manifest {
        let path = GuestPath::parse(path).unwrap();
        let r = Record {
            entry: "t".into(),
            path: path.clone(),
            sha256: digest(content.as_bytes()),
            source: user_source("t"),
        };
        Manifest { records: [(path, r)].into_iter().collect() }
    }

    fn only(p: &Plan) -> &Action {
        assert_eq!(p.items.len(), 1, "{:?}", p.items);
        &p.items[0].action
    }

    fn managed_wants(content: &str) -> GuestFiles {
        GuestFiles::managed(vec![Desired::direct(
            Entry::managed(
                "compose",
                "stack",
                "/opt/stack/compose.yaml",
                FileFormat::Yaml,
                "0644",
                "0:0",
                OVER,
            )
            .unwrap(),
            content.as_bytes().to_vec(),
        )
        .unwrap()])
        .unwrap()
    }

    #[test]
    fn paths_recorded_for_another_source_are_refused_by_name() {
        use crate::entry::managed_source;

        let path = GuestPath::parse("/opt/stack/compose.yaml").unwrap();
        let mut managed_record = Manifest::default();
        managed_record.records.insert(path.clone(), Record {
            entry: "stack".into(),
            path: path.clone(),
            sha256: digest(b"old"),
            source: managed_source("compose", "stack"),
        });
        let mut nodes = root_dirs();
        nodes.insert(path.to_string(), file("old", 0o444));

        // A user entry on a managed path is refused, and the manifest keeps
        // the managed record.
        let p = plan(&wants("/opt/stack/compose.yaml", "new", OVER), &managed_record, &nodes, false);
        match only(&p) {
            Action::Refused(why) => assert!(why.contains("managed/compose/stack"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(p.manifest.records[&path].source, "managed/compose/stack");

        // The reverse: a managed claim on a user-recorded path is refused too.
        let p = plan(&managed_wants("new\n"), &recorded("/opt/stack/compose.yaml", "old"), &nodes, false);
        match only(&p) {
            Action::Refused(why) => assert!(why.contains("user/t"), "{why}"),
            other => panic!("{other:?}"),
        }

        // Same source converges and records its source.
        let p = plan(&managed_wants("new\n"), &Manifest::default(), &root_dirs(), false);
        assert_eq!(only(&p), &Action::Create, "{p:?}");
        assert_eq!(p.manifest.records[&path].source, "managed/compose/stack");
    }

    const T: &str = "/etc/app/t";
    const KEEP: LocalEdits = LocalEdits::Keep;
    const OVER: LocalEdits = LocalEdits::Overwrite;

    /// A file in the guest (content, mode), the recorded content, the policy,
    /// `force`, the action, and the recorded content afterwards.
    type Row = (
        Option<(&'static str, u32)>,
        Option<&'static str>,
        LocalEdits,
        bool,
        Action,
        Option<&'static str>,
    );

    /// The file × record × policy table, for a path an entry wants
    /// ("new") and for one only the manifest has.
    #[test]
    fn decision_table() {
        let left = |a: &Action| matches!(a, Action::Left(_));
        // (file in the guest, recorded content, policy, force, action, recorded afterwards)
        let wanted: &[Row] = &[
            (None, None, KEEP, false, Action::Create, Some("new")),
            (None, Some("old"), KEEP, false, Action::Create, Some("new")),
            (Some(("new", 0o444)), None, KEEP, false, Action::InSync, Some("new")),
            (Some(("new", 0o444)), Some("old"), KEEP, false, Action::InSync, Some("new")),
            (Some(("old", 0o444)), Some("old"), KEEP, false, Action::Update, Some("new")),
            (Some(("old", 0o444)), Some("old"), OVER, false, Action::Update, Some("new")),
            (Some(("new", 0o666)), Some("new"), KEEP, false, Action::Update, Some("new")),
            (Some(("new", 0o644)), None, KEEP, false, Action::KeptLocalEdit, None),
            (Some(("edited", 0o444)), Some("old"), KEEP, false, Action::KeptLocalEdit, Some("old")),
            (Some(("edited", 0o444)), None, KEEP, false, Action::KeptLocalEdit, None),
            (Some(("edited", 0o444)), Some("old"), OVER, false, Action::Overwrite, Some("new")),
            (Some(("edited", 0o444)), Some("old"), KEEP, true, Action::Overwrite, Some("new")),
            (Some(("edited", 0o444)), None, OVER, false, Action::Overwrite, Some("new")),
        ];
        for (i, (f, rec, edits, force, action, after)) in wanted.iter().enumerate() {
            let mut nodes = root_dirs();
            if let Some((c, mode)) = f {
                nodes.insert(T.into(), file(c, *mode));
            }
            let old = rec.map(|c| recorded(T, c)).unwrap_or_default();
            let p = plan(&wants(T, "new", *edits), &old, &nodes, *force);
            assert_eq!(only(&p), action, "row {i}");
            assert_eq!(p.ops.len(), usize::from(action.writes()), "row {i}");
            let got = p.manifest.records.values().next().map(|r| r.sha256.clone());
            assert_eq!(got, after.map(|c| digest(c.as_bytes())), "row {i}");
        }
        // Only the manifest has it: the entry was removed, or its view is gone.
        for pubn in [
            GuestFiles::Nothing,
            GuestFiles::Entries(vec![Resolved::Absent { name: "t".into() }]),
        ] {
            for force in [false, true] {
                let run = |node: Option<Node>| {
                    let mut nodes = root_dirs();
                    if let Some(n) = node {
                        nodes.insert(T.into(), n);
                    }
                    let p = plan(&pubn, &recorded(T, "written"), &nodes, force);
                    assert!(p.manifest.is_empty());
                    (only(&p).clone(), p.ops.len())
                };
                assert_eq!(run(Some(file("written", 0o444))), (Action::Delete, 1));
                assert_eq!(run(None), (Action::Forget, 0));
                let (a, ops) = run(Some(file("edited", 0o444)));
                assert!(left(&a) && ops == 0, "{a:?}");
                let (a, ops) = run(Some(Node::Symlink));
                assert!(left(&a) && ops == 0, "{a:?}");
            }
        }
    }

    #[test]
    fn unsafe_paths_are_refused() {
        let pubn = wants(T, "new", OVER);
        let cases = [
            ("/etc/app", Node::Symlink, "symlink"),
            ("/etc/app", file("x", 0o644), "not a directory"),
            (T, Node::Symlink, "symlink"),
            (T, Node::Dir, "regular"),
            (T, Node::Other, "regular"),
        ];
        for (at, node, why) in cases {
            let mut nodes = root_dirs();
            nodes.insert(at.into(), node);
            let p = plan(&pubn, &Manifest::default(), &nodes, true);
            assert!(
                matches!(only(&p), Action::Refused(r) if r.contains(why)),
                "{at}: {:?}",
                p.items
            );
            assert!(p.ops.is_empty());
        }
    }

    #[test]
    fn holding() {
        let old = recorded(T, "written");
        let mut nodes = root_dirs();
        nodes.insert(T.into(), file("written", 0o444));
        // A refused entry holds its record; so does a document that cannot be read.
        let refused = GuestFiles::Entries(vec![Resolved::Refused {
            name: "t".into(),
            reason: "bad".into(),
        }]);
        for pubn in [refused, GuestFiles::Held("unreadable".into())] {
            let p = plan(&pubn, &old, &nodes, true);
            assert!(matches!(only(&p), Action::Refused(_)));
            assert!(p.ops.is_empty() && !p.changes(&old));
        }
        // An unsafe manifest directory holds everything.
        nodes.insert("/etc/pve-meta".into(), Node::Symlink);
        let p = plan(&wants("/root/t", "new", KEEP), &old, &nodes, true);
        assert!(
            p.ops.is_empty() && matches!(only(&p), Action::Refused(r) if r.contains("symlink"))
        );
        // Nothing without a record is ever deleted.
        let p = plan(&GuestFiles::Nothing, &Manifest::default(), &root_dirs(), true);
        assert!(p.items.is_empty() && p.ops.is_empty() && p.dirs.is_empty());
    }

    #[test]
    fn directories_moves_and_the_manifest_after_a_partial_commit() {
        let mut nodes = root_dirs();
        nodes.remove("/etc/pve-meta");
        let p = plan(&wants("/etc/app/conf/t", "new", KEEP), &Manifest::default(), &nodes, false);
        assert_eq!(p.dirs, ["/etc/app/conf", "/etc/pve-meta"]);

        let old = recorded(T, "written");
        let mut nodes = root_dirs();
        nodes.insert(T.into(), file("written", 0o444));
        let p = plan(&wants("/etc/app/u", "written", KEEP), &old, &nodes, false);
        let actions: Vec<_> = p.items.iter().map(|i| i.action.clone()).collect();
        assert_eq!(actions, [Action::Create, Action::Delete]);
        let paths = |m: &Manifest| m.records.keys().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(paths(&p.manifest), ["/etc/app/u"]);
        assert_eq!(paths(&p.manifest_after(&old, &[true, false])), ["/etc/app/t", "/etc/app/u"]);
        assert_eq!(paths(&p.manifest_after(&old, &[false, true])), Vec::<String>::new());
    }

    #[test]
    fn probe_paths_cover_wanted_and_recorded() {
        let got = probe_paths(&wants("/etc/app/t", "x", KEEP), &recorded("u", "y"));
        let want = ["/etc", "/etc/pve-meta", "/etc/app", "/etc/app/t", "/etc/pve-meta/u"];
        assert_eq!(got, want.into_iter().map(String::from).collect());
    }
}
