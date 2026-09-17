// ---------------------------------------------------------------------------
// compose: merge plain objects into one, member name by member name.
// ---------------------------------------------------------------------------

// TreePanel's methods are composed from three sets below (`PVE.meta.Doc`,
// `PVE.meta.TextCard`, and the tree itself), all sharing one `this`.
// `Ext.apply`-style merging would resolve a name defined in two of them by
// keeping whichever was applied last -- silently, the same way a write that
// does not check its digest silently overwrites somebody else's. The loser
// here is not a row, it is a method: one definition quietly replaces the
// other, and the dead one still looks used because its name is still in the
// file. Throwing on the collision turns that into a load-time error instead
// of a "why doesn't this do what the comment says" bug report.
PVE.meta.compose = function (...parts) {
    let out = {};
    parts.forEach(function (part) {
        Object.keys(part).forEach(function (key) {
            if (Object.prototype.hasOwnProperty.call(out, key)) {
                throw new Error('PVE.meta.compose: duplicate member "' + key + '"');
            }
            out[key] = part[key];
        });
    });
    return out;
};

// One API request on behalf of a component that may be destroyed before the
// answer lands: a tab switch tears the panel down while requests are in flight,
// and no callback may touch a destroyed one. Everything in `opts` goes to
// `API2Request` as given (method defaults to GET); `success` and `failure` are
// wrapped so they become no-ops once `owner` is gone.
PVE.meta.request = function (owner, opts) {
    let guard = (fn) => (fn ? (...args) => (owner.isDestroyed ? undefined : fn(...args)) : undefined);
    // Two-argument `Ext.apply`s, applied in order, so the guarded callbacks are
    // what wins. The three-argument form puts its last argument *under* the
    // config, which would have handed API2Request the unguarded originals.
    let req = Ext.apply({ method: 'GET' }, opts);
    Ext.apply(req, { success: guard(opts.success), failure: guard(opts.failure) });
    Proxmox.Utils.API2Request(req);
};

