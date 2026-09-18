// ---------------------------------------------------------------------------
// Shape: what describes one document (`shape::Shape`) -- which prefix governs a path
// and what its schema says there (schemas shadow, never merge, DESIGN §3). Owns the
// `GET /meta/prefixes` listing and caches what the core derives from it alone.
// ---------------------------------------------------------------------------

PVE.meta.Shape = function (prefixes) {
    let me = this;
    me.prefixes = prefixes || [];
    me.byPrefix = Object.create(null);
    me.prefixes.forEach((p) => (me.byPrefix[p.prefix] = p));
    me.cache = { names: null, index: null, governing: Object.create(null) };
};

// The shape of a guest document: the listing `GET /meta/prefixes?id=<vmid>` already
// resolved for it.
PVE.meta.Shape.of = (prefixes) => new PVE.meta.Shape(prefixes);

// The shape of a registry document: one schema, rooted at the document. The empty
// prefix is a prefix of everything and the least specific of all, so it governs
// the whole document without a special case anywhere.
PVE.meta.Shape.rooted = (schema) =>
    new PVE.meta.Shape(schema ? [{ prefix: '', selector: { all: true }, schema: schema }] : []);

PVE.meta.Shape.empty = () => new PVE.meta.Shape([]);

Object.assign(PVE.meta.Shape.prototype, {
    ask: function (fn, ...rest) {
        return PVE.meta.Core.call(fn, this.prefixes, ...rest);
    },

    // The prefixes that reach this document, most-specific first, by name.
    names: function () {
        let me = this;
        if (me.cache.names === null) {
            me.cache.names = me.ask('shape_prefixes');
        }
        return me.cache.names;
    },

    // ... as the listing entries they came from (a failed one never appears).
    declared: function () {
        let me = this;
        return me.names().map((name) => me.byPrefix[name]);
    },

    // Whether anything here carries a schema at all -- if not, there are no
    // findings and no hovers to compute.
    hasSchema: function () {
        return this.declared().some((d) => d.schema);
    },

    // The prefix governing `path`, as its listing entry, or `null`. No production
    // caller (the row builder uses `schemaIndex`); kept as the cross-boundary test
    // of `shape_governing`'s most-specific-wins rule.
    governing: function (path) {
        let me = this;
        let hit = me.cache.governing;
        if (!(path in hit)) {
            hit[path] = me.ask('shape_governing', path);
        }
        return hit[path] === null ? null : me.byPrefix[hit[path]];
    },

    // Every declared path with its schema node, parents first, pruned where a
    // more specific prefix governs: `[{ path, prefix, schema }]`.
    schemaIndex: function () {
        let me = this;
        if (me.cache.index === null) {
            me.cache.index = me.ask('shape_schema_index');
        }
        return me.cache.index;
    },

    // Everything in `doc` that does not match its governing schema: `[{ path, msg }]`,
    // in the core's order. A `format` comes back undecided and is checked here with
    // proxmoxlib's own validator, resolved in place so nothing is sorted twice.
    findings: function (doc) {
        let out = [];
        this.ask('shape_findings', doc).forEach(function (r) {
            if (r.msg !== undefined) {
                out.push(r);
                return;
            }
            let msg = PVE.meta.Utils.checkFormat(r.format, r.value);
            if (msg) {
                out.push({ path: r.path, msg: msg });
            }
        });
        return out;
    },
});

