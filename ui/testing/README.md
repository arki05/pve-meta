# Headless browser checks

Puppeteer scripts used to verify the editor page against a lab node (PVE 9.2, root
password known, `puppeteer-core` + system Chromium on the build host). They log in
through the ticket API, open the Metadata tab of a guest and the standalone page, and
assert behaviour: `meta-ui-lib.js` (shared helpers), `meta-ui-write-check.js` (edit →
diff → Apply round trip), `meta-ui-race-check.js` (view-switch race), `meta-ui-draft-check.js`
(draft protection), `meta-ui-r3-fix-check.js` (selected view disappears),
`meta-ui-q2-check.js` (scope revoked while open), `meta-ui-reload-invalidate-check.js`
(stale answers after Reload), `meta-ui-check.js`/`ui-check.js`/`ui-write-check.js`
(earlier variants), `check-tab*.js` (tab injection). Run from a directory containing
`node_modules/puppeteer-core`; each script names the node it targets near the top.
Scripts that mutate CT 200's document restore it afterwards.
