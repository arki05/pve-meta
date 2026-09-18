/*
 * pve-meta-tree.js — the native ExtJS implementation of the pve-meta editor.
 *
 * One panel, one edited document, two views of it (DESIGN.md §8):
 *
 *   Tree — an Ext.tree.Panel with columns Key | Value | Description over the document
 *     the caller can see. Rows are the union of the keys present in the document and
 *     the keys the governing prefixes declare (`GET /meta/prefixes`, matched by prefix
 *     and selector against this guest, most-specific first -- schemas shadow, they
 *     never merge); a declared-but-unset key renders faded with its default, and
 *     "setting" it is just editing it. Map rows carry a folder icon (open when
 *     expanded), value rows a document icon, both at the size and colour of the PVE
 *     resource tree. Comment keys (`k__`, and the bare `__` for the map itself) are not
 *     rows — `k__` is the Description of row `k`. A list is a container like a map, one
 *     row per member.
 *
 *   Text — a full-document Monaco editor (YAML, with a presentation-only YAML/JSON view
 *     toggle) over the same document, as a buffer of the file's own text.
 *
 * The Tree|Text segmented button in the footer (`PVE.meta.Footer`) swaps the body in
 * place. The buffer is the only unwritten state in the editor, so leaving Text with
 * one asks before dropping it; everything the tree does is written as it is done.
 *
 * `PVE.meta.TreePanel` is composed at definition time from three method sets that
 * share one `this` (`PVE.meta.compose`, below the other `PVE.meta.*` helpers):
 * `PVE.meta.Doc` owns one document's transport and state -- the API calls, the
 * digest, the cached data; `PVE.meta.TextCard` owns the Text card and the Tree|Text
 * switch; the `Ext.define` body itself is left with the tree -- rows, the toolbar,
 * the columns, the editors. Three responsibilities in one class is what made a
 * change in one keep going wrong because of another; the split names the seams
 * instead of letting them stay implicit in which third of the file a method
 * happened to live in.
 *
 * Editing is a modal row editor (Edit, double-click, or Enter), the field chosen from
 * the grammar type and falling back to the value's own type; a row is editable iff
 * `GET /meta/access` says the document is writable (DESIGN §4) -- nothing is decided
 * per path. **An edit is one write with the digest** (DESIGN §8), built by
 * `Doc.writeFor` and followed by a reload:
 *   PUT /meta/guests/{vmid}?view=<the row's path>&mode=replace&data=<json>&digest=<d>&comments=1
 * and `DELETE ?view=<path>&digest=<d>` for a removal. Every read and write of a
 * document carries `comments=1`: the comment keys are the description column, and
 * without it the server would leave them out.
 * 409 (digest mismatch) reloads and reports the API's message verbatim -- the digest
 * check on every write is what catches a concurrent change; there is no background
 * poll, only the toolbar's manual Reload. 422 is an enforcing prefix refusing the
 * write, naming the paths; "Save anyway" sends it again with `force=1`.
 *
 * Monaco has three jobs: "Edit selection as text" on the selected subtree, the Text
 * card on the whole document, and the diff behind the Text card's Diff button. Its AMD
 * loader is fetched lazily on first use from /pve2/js/pve-meta-extjs/vs/loader.js
 * (Monaco is vendored into the package by `make ui`, never fetched from a CDN); every
 * editor is disposed when its owner goes away.
 *
 * The rules -- YAML in and out, key names, which prefix governs a path, what a schema
 * makes of a value -- are not implemented here. They are pve-meta-core, the server's
 * own crate, built for the browser (crates/pve-meta-wasm, loaded lazily as
 * `PVE.meta.Core`). One of them is an object the panel holds -- a `PVE.meta.Shape`
 * per document, which owns the prefix listing and caches what the core derives from
 * it -- and the rest are stateless faces: `Codec`, and the key-name checks on
 * `Utils`. The server stays the authority: a write sends the buffer or one subtree to
 * the API, which runs the same code again on the real write.
 *
 * pve-ext's page loader loads this file and instantiates `pveMetaTreePanel` as the
 * tab (see README.md), so session, CSRF, dark theme and i18n all come from the PVE
 * UI — none of it is reimplemented here. Plain ES2017. Written as `src/*.js`, one
 * file per section in the order their two-digit prefixes give, and shipped as the
 * one file `make js` concatenates them into, because the loader wants one script
 * URL; no bundler and no transform stand between the two.
 */

Ext.ns('PVE.meta');

