# 023 — Publishing into containers is a separate package that pushes files one way

**Status:** accepted (2026-09-17).

## Context

The store holds intent and operators act on it, but the most common operator is a
service inside the guest the document describes: llama-swap reading its model list, a
Traefik reading its dynamic file. Each needed its own small tool to carry a subtree
from `/etc/pve/meta` into a container. Clouds answer the same need with an instance
metadata service the guest asks. pve-compose already carries a document into its
containers by rendering a file and pushing it with `pct`, and that has held up.

pve-meta itself runs nothing on its own: the API answers requests, the hooks run inside
PVE's own operations, and there is no sweeper (009). Carrying documents into guests
needs something that does run: a container that was stopped when its document changed
has to be written when it starts, and a file edited inside the guest is only seen by
looking.

## Decision

A third binary package from this source, `pve-meta-publish`, depending on `pve-meta` at
the exact version. A daemon on every node reads each local container's `publish` key
through `pve-meta-core`, renders the views it names, and writes them with `pct exec`,
content and all on the script's stdin: on a change of the container's document digest, on container start, and
every ten minutes for drift. One way, host to guest.

The guest's own manifest, `/etc/pve-meta/.published`, records what was written, by
hash, and is written after the files, from what was done. A file is replaced or deleted only while it still hashes to its record; one edited
in the guest, or one the daemon never wrote, is kept unless the entry says `overwrite`
or an operator runs `sync --force`, and never deleted. A store or vmlist that cannot be
read is an unknown, never an empty answer: the daemon does nothing until it can read it.
The manifest lives in the guest because the state it describes does: it travels with a backup, a restore and a
migration, and nothing about it needs the cluster.

Paths may be absolute. Writing `publish` is root inside the guest whatever the root
directory is, so confining files to `/etc/pve-meta` bought no security and lost the use
this exists for: driving the configuration file of software that reads it from its own
place.

Nothing is written back into the document, and nothing runs in the guest after a write.

The trust boundary is pve-meta's own: who may write the document. Whoever can write a
container's `publish` key is trusted as root in it, the trust `pct enter` and `pct exec`
already carry, and access to the key (permission files, `VM.Config.Options`) is the gate.
pve-meta-publish does not defend against users or processes inside the guest. What it
does guard against is accidents, never overwriting or deleting a file it did not write or
that was edited and never writing anywhere but where the document names, and a broken or
hung container wedging or bloating the host daemon.

## Alternatives

* **In the `pve-meta` package.** Every install would run a root daemon that execs into
  every container, for a feature most installs do not use, and the store would stop
  being the passive thing 009 made it. A separate package makes installing it the
  opt-in, and its unit its own.
* **Synchronously in the API write path.** A `PUT` would wait in pvedaemon, holding the
  document's cluster lock, on `pct` calls into a container that may be slow, stopped, or
  on another node; a failure has no place in a write that already succeeded; and a
  container that starts later, or a file edited in the guest, still needs a loop. With
  the loop there anyway, the write path gains nothing but latency and a new way to fail.
* **A read-only bind mount of a host directory.** Rendered files under a host
  directory, mounted into the container as an `mpN`. Adding it is a config change and a
  container restart, an unprivileged container sees host root's files as
  `nobody:nogroup`, a mount point is one directory rather than the path software
  already reads, and a migration needs the host directory on the target node first. A
  bind mount is also outside what vzdump backs up.
* **A guest-side metadata service**, in the manner of a cloud's. It needs a network
  path from every guest to its node, which PVE bridges do not give by default and a
  firewall may forbid; a way to know which guest asks, which a bridged guest can spoof;
  and a client in every guest for software that already reads files. Files are the
  interface every service has.
* **Two-way sync.** A guest-side edit written back into the document would make the
  guest's root a writer of the store, past permissions and the audit line (015), and two
  writers of one file need conflict resolution. The store holds intent; a local edit is
  a fact about the guest, reported by `status`.
* **Defend against users inside the guest.** Requiring every directory above a target to
  be root-owned and writable by nobody else, re-checking that at commit, and creating
  temp files exclusively would keep an unprivileged guest user from redirecting a write.
  pve-meta's trust boundary is who may write the document, and a guest's own users are on
  the other side of the container, not of the document; the ownership rule also refused
  ordinary service-owned directories such as `/opt/app`.
* **Status written back into the document.** The daemon on every node would become a
  writer, every sync would move the version token and wake every poller, and change
  the digest of the very document the daemon watches, observed state would collide by
  digest with an operator's writes of intent, and the daemon would need a permission of
  its own. `status` computes it live instead.

## Consequences

* **`publish` is root in the guest.** Whoever may write a guest's document (full write,
  `VM.Config.Options`, or an `rw` rule on `publish`) can place any file with any owner
  and mode at any admitted path, and with `overwrite` replace one the guest has. Grant
  it as that.
* **Accidents, not adversaries.** A path with a symlink or a file above the target, or a
  target that is not a regular file, is refused; a user inside the guest who swaps a
  directory for a symlink between the probe and the commit can still redirect a write,
  and that is out of scope. An edit made between a file's hash check and its rename is
  replaced.
* **The manifest can fall one write behind.** It is written after the files, so a daemon
  killed between a rename and the manifest write, followed by a document change, leaves
  that file looking like a local edit: kept, never clobbered, until `sync --force`.
  Writing an intent manifest before the files closed that and cost a second manifest
  write and its bookkeeping on every sync; the failure it prevents is a kept file, not a
  lost one.
* **Each sync costs Perl.** Every `pct` call starts an interpreter: two for a sync that
  changes nothing, which each watched running container gets every ten minutes, and four
  for one that writes. `pct exec` leaves no task-log row, where a `pct push` per file
  left one each. A change lands within a poll (10 s) plus that. The daemon does not look
  into containers without a `publish` key, so a key removed while it is not running
  leaves its files.
* **Containers only.** A VM has no `pct`; a VM's `publish` key is ignored. The guest needs
  a POSIX shell, `stat`, `sha256sum` and `base64`, which coreutils and busybox provide.
* **Directories the daemon created stay** when the files in them are deleted, and the
  schema cannot describe an entry's fields (the dialect has no typed map members), so
  those are checked by the daemon and a refusal is seen in the log and in `status`, not
  at write time.
