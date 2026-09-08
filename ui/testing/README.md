# Headless browser checks

Puppeteer scripts that drive the tree page against the lab node (PVE 9.2, `puppeteer-core`
+ system Chromium on the build host). They log in through the ticket API and then reach
the page the way a user does — the rendered grid, the toolbar, the dialogs — never through
Yew or the store.

| Script | What it asserts |
|---|---|
| `meta-ui-lib.js` | Shared helpers: ticket, raw API calls, `rows()`, `clickRow`, `clickButton`, `setField`/`pickCombo` (pwt fields carry no DOM `name`; they are found by their `aria-labelledby` label), Monaco readers. |
| `meta-ui-tree-check.js` | Rendering: alphabetical rows, nesting, comment keys as descriptions, arrays as one leaf, the Owner column, and the toolbar's per-row enablement. |
| `meta-ui-write-check.js` | Edit a scalar and its description, Add (typed), Remove — each verified against the API, including the shape of the `PUT`/`DELETE`. |
| `meta-ui-grammar-check.js` | Declared-but-unset rows: greyed, with default, description, owner and the "set" action that writes them. Skips itself when no registration on the cluster carries a grammar. |
| `meta-ui-text-check.js` | "Edit as text": the subtree it edits, the YAML/JSON toggle, the Monaco diff confirmation, and the `text` replace it applies. |
| `meta-ui-live-check.js` | The 5 s version poll refreshing the tree, the poll holding still while a dialog is open, a 409 reloading with a notice, and a scoped principal's per-row editability. |
| `meta-ui-shots.js` | The screenshots in `../docs/screenshots/` (embedded in the PVE tab and standalone, light and dark, plus the dialogs). |
| `check-tab*.js` | The `pve-ext` tab injection itself, not this page. |

Run from a directory that has `node_modules/puppeteer-core` (`/root/headless` on the build
host):

```sh
rsync -az ui/testing/ pve-meta-build:/root/headless/
ssh pve-meta-build 'cd /root/headless && node meta-ui-tree-check.js'
```

Each script takes `[host] [vmid]` and seeds the document it needs first, so a run is
independent of whatever the document happened to hold. The write-shaped checks default to
CT 201 and the read-only ones to a `scoped@pve` login; they say so and skip when the lab
does not have them.
