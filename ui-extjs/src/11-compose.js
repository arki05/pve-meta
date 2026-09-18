// ---------------------------------------------------------------------------
// compose: merge plain objects into one, member name by member name.
// ---------------------------------------------------------------------------

// TreePanel's methods are composed from three sets (`PVE.meta.Doc`, `PVE.meta.TextCard`,
// the tree itself) sharing one `this`. Throws on a duplicate member name rather than
// letting `Ext.apply`-style merging silently keep whichever was applied last.
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

// One API request on behalf of a component that may be destroyed before the answer
// lands (a tab switch tears the panel down mid-flight): `success`/`failure` become
// no-ops once `owner` is gone. Everything else in `opts` goes to `API2Request` as given.
PVE.meta.request = function (owner, opts) {
    let guard = (fn) => (fn ? (...args) => (owner.isDestroyed ? undefined : fn(...args)) : undefined);
    // Two two-argument `Ext.apply`s, so the guarded callbacks win over the originals.
    let req = Ext.apply({ method: 'GET' }, opts);
    Ext.apply(req, { success: guard(opts.success), failure: guard(opts.failure) });
    Proxmox.Utils.API2Request(req);
};

