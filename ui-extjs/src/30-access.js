// ---------------------------------------------------------------------------
// Access: who may touch a path (`scopes::Effective`).
//
// A `GET /meta/access` answer is the caller's effective access to one document;
// the same struct the server builds per request, read here from the wire.
// Permissions accumulate by containment, and a rule on `p` covers `p`, its
// comment key `p__` and everything under `p.` -- the only comment-key rule
// there is (DESIGN §4).
// ---------------------------------------------------------------------------

//
// **Every answer here fails closed before the core has loaded**: nothing is
// covered, nothing is writable, no rule reaches. The core is fetched lazily and
// the panel syncs its buttons on render, before the first document read has
// even started; an answer that threw instead aborted that render hook and left
// the panel empty until a reload nobody knew to ask for. A guard in one caller
// (`editableFor`) is a guard the next caller forgets, so it lives here.
PVE.meta.Access = {
    covers: (prefix, path) => PVE.meta.Core.loaded() && PVE.meta.Core.call('covers', prefix, path),

    canWrite: (access, path) =>
        PVE.meta.Core.loaded() && PVE.meta.Core.call('access_can_write', access || {}, path),

    // May this caller write *anything* here: full write, or at least one `rw`
    // scope. What a write may actually change is decided by what it changes
    // (DESIGN §5); this only says whether there is any point offering Apply.
    hasAnyWrite: (access) =>
        PVE.meta.Core.loaded() && PVE.meta.Core.call('access_has_any_write', access || {}),

    // Every rule, from every permission file in the `GET /meta/permissions`
    // listing, whose selector matches a guest carrying `tags` -- with the file it
    // came from: `{ name, authid, prefix, mode, selector }`. A file that did not
    // load grants nothing.
    rulesReaching: (permissions, tags) =>
        PVE.meta.Core.loaded() ? PVE.meta.Core.call('rules_reaching', permissions || [], tags || []) : [],
};

