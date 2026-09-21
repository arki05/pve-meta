/*
 * pve-meta-tree.js — the ExtJS editor for one metadata document (DESIGN.md §6).
 * A tree of rows (`PVE.meta.TreePanel`, composed from `PVE.meta.Doc`'s transport
 * and `PVE.meta.TextCard`'s Monaco view) and a whole-document Monaco buffer,
 * switched by the footer. A tree edit is one write with the digest and a
 * reload; the Monaco buffer is the only unwritten state, applied by its own
 * Apply. The rules -- YAML, key names, prefixes, schemas -- are pve-meta-core
 * (loaded as wasm, `PVE.meta.Core`); the server re-checks every write. Built
 * from `src/*.js`, one file per section, concatenated by `make js`.
 */

Ext.ns('PVE.meta');

// Unset rows dim relative to the theme instead of taking a fixed grey: PVE
// ships no theme-aware faded class, and a fixed rgb is pale-on-light and
// near-invisible-on-dark (lab report 1, finding 4).
Ext.util.CSS.createStyleSheet('.pve-meta-faded { opacity: 0.55; }', 'pve-meta-faded');

