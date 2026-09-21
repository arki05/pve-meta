# pve-meta-guest-files — specification

A separate binary package from this source, `pve-meta-guest-files`, writes views of a
container's own document into that container as files: one way, host to guest, so a
service inside reads configuration kept in pve-meta; nothing of this runs in pveproxy,
pvedaemon or the write path. **Writing a guest's `guest-files` key is root inside that
guest**: it chooses the content, mode and owner of a file at any admitted path, and with
`local_edits: overwrite` that includes a file the guest already had, `/etc/shadow` among
them. So `VM.Config.Options` on a container is root inside it, the trust `pct enter`
already carries.

```yaml
guest-files:
  swap:                          # the entry name: any key
    view: llm.swap               # required: a dotted view into this guest's document
    path: llm/llama-swap.yaml    # required: relative to /etc/pve-meta, or absolute
    format: yaml                 # yaml (default) | json | raw
    mode: "0444"                 # default; permission bits only
    owner: "0:0"                 # default; uid:gid as seen inside the guest
    local_edits: keep            # keep (default) | overwrite
```

* **The prefix.** `/usr/share/pve-meta/prefixes/guest-files.yaml`: `selector: { all: true
  }`, `enforce: true`; a cluster file of the same name overrides the packaged one (§3),
  with per-node differences as a `nodes:` map inside that file. A
  write is refused only when `guest-files` is not a map; the daemon checks each entry's
  fields.
* **Content.** `yaml` is the store's canonical dump of the view, `json` pretty JSON with
  a trailing newline, `raw` the view verbatim (must be a string). Comment keys (§2) are
  stripped from a `yaml` or `json` view at every depth. Content is bounded by the
  store's own write cap (§5); over it, the entry is refused. A missing view means no
  file, as if the entry were removed.
* **Entries are judged one by one.** An entry that fails to validate — an unknown or
  invalid field, `raw` on a non-string, content over the cap — is refused and holds what
  it wrote before. Two entries resolving to one path, or one to a directory the to a directory the
  other's path runs through, are both refused. A document that does not parse, or a
  `guest-files` that is not a map, holds everything.
* **Paths.** A relative path resolves under `/etc/pve-meta`, an absolute one is used as
  is. Every segment is of the key charset (§2) plus `.`, never empty, `.` or `..`, never
  carrying `.pve-meta-guest-files` (the temp mark); nothing is normalised, and a path has no
  length cap of its own. Refused: `/`, anything under `/proc`, `/sys` or `/dev`, the
  manifest and the directories it lives in. `/run` is allowed.
* **Where a write lands.** Every existing path above a target has to be a directory, not
  a symlink or a file, and the target a regular file or nothing, or the entry is refused
  — so a file lands exactly where named or not at all, and only a regular file is ever
  replaced or removed. `/var/run` is a symlink on current distributions; use `/run`.
  `/etc/pve-meta` must pass too, since the manifest lives there; when it does not,
  everything is held. Missing directories are created root `0755`, never removed.
* **The manifest**, `/etc/pve-meta/.guest-files`, root `0600`, JSON, `{ version: 1, files:
  [...] }`: one record per file written, with the entry name, the absolute path and the
  sha256 of the content as written. A file is **ours** while it hashes to its record,
  and moves with the guest through backup, restore and migration. Above 1 MiB, not
  parsing, or holding a record whose path does not validate, distrusts the whole
  manifest: nothing in it is deleted, and files exactly as wanted are adopted again. The
  entry name and stored sha256 are the daemon's own and are otherwise unchecked.
* **Per path an entry wants** (the file `F`, the content `D`):

  | the file | | action |
  |---|---|---|
  | missing | | created, record or not: it is desired state |
  | `F = D`, mode and owner as wanted | | in sync; recorded |
  | ours | | updated |
  | not ours, or no record | `keep` | kept, not touched, logged once |
  | not ours, or no record | `overwrite`, or `sync --force` | overwritten, logged |
  | unsafe | | refused |

  Already-exact content is adopted by being recorded; the same content under another
  mode or owner, unrecorded, is a local edit.

  **Per path only the manifest has** (entry removed, or its view gone): ours is deleted;
  not ours, or unsafe, is left in place and dropped from the manifest, even with
  `--force`; missing is dropped. Only files with a record are ever deleted.
* **A sync** is `pct exec` calls of generated shell scripts on `/bin/sh -s`: read the
  manifest, probe every path needed, decide the plan, then one commit that makes missing
  directories, writes each file's content (base64) to a temp sibling under `umask 077`,
  and renames it over the target only if it still hashes to what the probe saw (or is
  missing); a removal only if the file still hashes to its record. Each operation
  succeeds, is skipped (changed underneath it), or fails, independently of the rest; the
  manifest is written last from what was done. The guest needs a POSIX shell and
  coreutils. A `pct` call is killed with its process group after 120 s, or past 4 MiB
  stdout or 64 KiB stderr. Guest-produced text in an error is `{:?}`-formatted, so it
  stays on one log line.
* **The daemon**, `pve-meta-guest-files daemon`, runs as root on every node, on the
  containers the vmlist places there. Every 10 s it reads the vmlist, the running
  containers and each local document's digest; **a read error is never an absent
  document** — if any of those cannot be read, including pmxcfs being unmounted (§5),
  the poll ends with nothing synced, logged once. A container is watched while its
  document has `guest-files`, or its manifest still has records. A watched, running
  container is synced when unsynced, changed, restarted, 10 minutes after its last sync,
  or 60 s after a failed one; a stopped container waits until it starts, a locked one
  (`lock:`) until it clears. One `flock` per guest under `/run/lock/pve-meta-guest-files` is
  shared with `sync`, which logs every write and every lasting state once.
* **No hooks.** Nothing runs in the guest after a write; a service picks up the change
  itself.

## Managed files (for operators)

The same writer, without a document: an operator (pve-compose today) hands
content over through the crate (`Entry::managed`, `Desired::direct`,
`GuestFiles::managed`) and syncs it with the same `inspect`/`sync`, the
same `GuestLock`, and the same manifest. What differs:

* `view` is none and `format` ignored: the content is given, never rendered.
* `source` is `managed/<operator>/<name>` instead of `user/<entry>`, and is
  recorded in the manifest. A path the manifest attributes to another source
  is refused to the claimant by name (`user/` entries among themselves keep
  the old behaviour, so renaming an entry keeps its path).
* `GuestFiles::managed` refuses colliding paths up front, like entries do.
* Tag lists go through `node::split_tags`, the API's rule, not a private copy.

There is no API endpoint for managed files: effects stay out of pveproxy,
and each operator keeps its own loop and calls the library synchronously.

| Command | Does |
|---|---|
| `daemon` | the loop above |
| `sync <vmid> [--force]` | one sync of a running, unlocked container on this node now, under its lock; `--force` overwrites its local edits once |
| `status [<vmid>]` | per entry what a sync would do, in the words the log uses, checked in the guest now; a stopped or locked container is one row; every container on this node with `guest-files` by default |
