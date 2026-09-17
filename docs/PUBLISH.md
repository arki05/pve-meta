# pve-meta-publish — specification

A separate binary package from this source, `pve-meta-publish`, writes views of a
container's own document into that container as files: one way, host to guest, so a
service inside reads configuration kept in pve-meta. The `pve-meta` package gains nothing
that runs; the store stays passive, and nothing of this is in pveproxy, pvedaemon or the
write path. **Writing a guest's `publish` key is root inside that guest**: it chooses the
content, mode and owner of a file at any path the rules below admit, and with
`local_edits: overwrite` that includes a file the guest already had, `/etc/shadow` among
them. A permission rule granting `rw` on `publish` grants that on every guest its selector
matches.

```yaml
publish:
  swap:                          # the entry name: any key
    view: llm.swap               # required: a dotted view into this guest's document
    path: llm/llama-swap.yaml    # required: relative to /etc/pve-meta, or absolute
    format: yaml                 # yaml (default) | json | raw
    mode: "0444"                 # default; permission bits only
    owner: "0:0"                 # default; uid:gid as seen inside the guest
    local_edits: keep            # keep (default) | overwrite
```

* **The prefix.** The package ships `/usr/share/pve-meta/prefixes/publish.yaml`:
  `selector: { all: true }`, `enforce: true`, and a schema of `type: object` whose
  description names the fields; a cluster or node file of the same name overrides it
  (DESIGN.md). The dialect cannot describe the members of a map whose keys it does not know, so
  a write is refused only for a `publish` that is not a map; the fields are checked by
  the daemon.
* **Content.** `yaml` is the store's canonical dump of the view, `json` pretty JSON with a
  trailing newline, `raw` the view verbatim, which must then be a string. Comment keys
  (DESIGN.md) are removed from a `yaml` or `json` view at every depth before it is rendered.
  Content is bounded by the store's own write cap (DESIGN.md), which a view of a document
  written through the API is always inside; an entry whose rendered content is above it
  is refused, which a document written out of band or `json`'s indentation can reach. A
  view the document does not have means no file, the same as the entry being removed.
* **Entries are judged one by one.** An entry is refused for an unknown field, a missing
  `view` or `path`, a `view` that is not a dotted key path, a `format`, `mode`, `owner` or
  `local_edits` that is not one of the above, `raw` on a non-string, content over the cap,
  or a path refused below. Two entries resolving to one path, or one to a directory the
  other's path runs through, are both refused. A refused entry holds whatever it
  published before. A document that does not parse, or a `publish` that is not a map,
  holds everything. Comment keys are neither entries nor fields. An `owner` the guest
  cannot give a file (an id outside an unprivileged container's range) is not refused
  here; its write fails in the guest.
* **Paths.** A relative path resolves under `/etc/pve-meta`, an absolute one is used as
  it is. Every segment is of the key charset (DESIGN.md) plus `.`, none empty, `.` or `..`, none
  containing `.pve-meta-publish` (the temp names); at most 1024 bytes resolved; nothing is
  normalised. Refused: `/`, anything under `/proc`, `/sys` or `/dev`, the manifest
  `/etc/pve-meta/.published` and the directories it lives in. `/run` is allowed.
* **Where a write lands.** When the guest is probed, every existing path above a target
  has to be a directory, not a symlink or a file, and the target a regular file or
  nothing; otherwise the entry is refused, so a file is written where the document names
  it or not at all, and nothing but a regular file is ever replaced or removed. Who owns
  a directory does not matter. `/var/run` is a symlink on current distributions, so a
  file there is refused and `/run` is the path to use. `/etc/pve-meta` has to pass as
  well, whatever the paths, since the manifest is written there; when it does not,
  everything is held. Missing directories are created root `0755`, for absolute paths as
  for relative ones, and are never removed.
* **The manifest**, `/etc/pve-meta/.published`, root `0600`, JSON, `{ version: 1, files:
  [...] }`: one record per file written, with the entry name, the absolute path and the
  sha256 of the content as written. A file is **ours** while it hashes to its record. It
  lives in the guest, so it travels with the container through backup, restore and
  migration. A manifest above 1 MiB is an error. One that does not parse, or has any
  record whose path, entry name or hash does not validate, is not trusted at all: nothing
  in it is deleted, and files exactly as wanted are adopted again. Nothing about
  publishing is written back into the document.
* **Per path an entry wants** (the file `F`, the content `D`):

  | the file | | action |
  |---|---|---|
  | missing | | created, record or not: it is desired state |
  | `F = D`, mode and owner as wanted | | in sync; recorded |
  | ours | | updated |
  | not ours, or no record | `keep` | kept, not touched, logged once |
  | not ours, or no record | `overwrite`, or `sync --force` | overwritten, logged |
  | unsafe | | refused |

  A file that is already exactly what an entry wants, content, mode and owner, is adopted
  by being recorded, and is deleted like any other when the entry is later removed; the
  same content with another mode or owner and no record is a local edit.

  **Per path only the manifest has** (entry removed, or its view gone): ours is deleted;
  not ours, or unsafe, is left where it is and dropped from the manifest, even with
  `--force`; missing is dropped. Only files with a record are ever deleted.
* **A sync** is `pct exec` calls of generated shell scripts fed to `/bin/sh -s` on stdin:
  one reads the manifest (`head -c`, capped), one probes every path it needs, and the plan
  is decided. When a file changes, one commit creates the missing directories, then for
  each file writes its content, carried in the script as base64, to a new
  `.<name>.pve-meta-publish.tmp` beside its target, created under `umask 077` so that a
  file meant to be `0600` is never readable by others before its mode is set, gives it its
  owner and mode, and renames it over the target only if the target still hashes to what
  the probe saw (or is still missing); each removal happens only if the file still hashes
  to its record. Each operation is done, not done because the file changed, or failed
  (writing, owning, renaming), and the others go ahead. The manifest is written last the
  same way, from what was done: a failed or not-done operation keeps its old record. A
  file edited between the probe and the commit is not touched, and the next sync sees a
  local edit; one made in the moment between the hash check and the rename is replaced. A
  daemon killed between a rename and the manifest write, followed by a document change,
  leaves that file looking like a local edit: kept, never clobbered, until `sync --force`.
  Nothing is written when content, mode and owner already match. The guest needs
  `/bin/sh`, `stat -c`, `sha256sum`, `base64 -d`, `head`, `cut`, `mv`, `rm`, `mkdir`,
  `chown` and `chmod`. Every `pct` call runs in its own process group and is killed with
  it after 120 s, or once its stdout passes 4 MiB or its stderr 64 KiB, so a hung or
  broken container cannot wedge or bloat the daemon. Guest-written text is escaped in logs
  and on the terminal.
* **The daemon**, `pve-meta-publish daemon`, runs as root on every node from
  `pve-meta-publish.service`, enabled on install. It acts only on containers the vmlist
  places on its node. Every 10 s it reads the vmlist, the running containers (the LXC
  command sockets `@/var/lib/lxc/<vmid>/command` in `/proc/net/unix`, with their inodes)
  and the digest of each local container's document. **A read error is never an absent
  document**: when any of those cannot be read, including the store refusing because
  pmxcfs is not mounted (DESIGN.md), the poll ends there with nothing synced, logged once. It
  watches a container whose document has `publish`, and one whose manifest had records
  at its last sync, so a key removed while the daemon runs has its files cleaned up; a key
  removed while the daemon is down leaves its files. A running, watched container is
  synced when the daemon has not synced it yet or its document's digest moved; when it
  was not running at the previous poll or its command socket's inode changed (a
  restart); and 10 minutes after its last sync, or 60 s after a failed one. A stopped
  container is skipped until it starts, a container whose config has `lock:` (backup,
  snapshot, migration) until the lock is gone. VMs are not looked at. One `flock` per
  guest under `/run/lock/pve-meta-publish` is shared with `sync`; a daemon that finds it
  held tries at the next poll, `sync` waits. It logs to the journal: every write that was
  done, with the reason for the sync, and every lasting state (a local edit kept, a
  refusal, a failing write or sync, a lock, the store unavailable) once when it begins.
* **No hooks.** Nothing runs in the guest after a write; a service picks a change up
  itself (a file watch, a systemd path unit).

| Command | Does |
|---|---|
| `daemon` | the loop above |
| `sync <vmid> [--force]` | one sync of a running, unlocked container on this node now, under its lock; `--force` overwrites its local edits once |
| `status [<vmid>]` | per entry what a sync would do, in the words the log uses, checked in the guest now; a stopped or locked container is one row; every container on this node with `publish` by default |

