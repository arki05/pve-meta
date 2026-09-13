// ---------------------------------------------------------------------------
// Shape: what describes one document (`shape::Shape`).
//
// The prefixes that reach a guest, most-specific first; which of them governs a
// path; what its schema says about the value there. Schemas shadow, they never
// merge (DESIGN §3). A registry document is shaped by its meta-schema rooted at
// the document itself.
//
// A Shape owns its two inputs -- the `GET /meta/prefixes` listing and the guest's
// tags -- and caches what the core derives from them alone (the applicable
// prefixes, the schema index, each `governing` answer). Only `findings` takes a
// document, so only `findings` goes to the core every time. The panel keeps one
// Shape per document and drops it when the listing, the tags or the meta-schema
// change (`shapeFor`), which is what makes a render two core calls rather than
// a dozen. Nothing is held inside the wasm between calls: the core rebuilds its
// own Shape from the listing on each question, which costs microseconds and no
// lifetime for this side to manage.
// ---------------------------------------------------------------------------

PVE.meta.Shape = function (prefixes, tags) {
    let me = this;
    me.prefixes = prefixes || [];
    me.tags = tags || [];
    me.byPrefix = Object.create(null);
    me.prefixes.forEach((p) => (me.byPrefix[p.prefix] = p));
    me.cache = { names: null, index: null, governing: Object.create(null) };
};

// The shape of a guest document: the listed prefixes (loaded or not), the guest's
// tags.
PVE.meta.Shape.of = (prefixes, tags) => new PVE.meta.Shape(prefixes, tags);

// The shape of a registry document: one schema, rooted at the document. The empty
// prefix is a prefix of everything and the least specific of all, so it governs
// the whole document without a special case anywhere.
PVE.meta.Shape.rooted = (schema) =>
    new PVE.meta.Shape(schema ? [{ prefix: '', selector: { all: true }, schema: schema }] : [], []);

PVE.meta.Shape.empty = () => new PVE.meta.Shape([], []);

// Of the findings `after` has, those an edit is answerable for: not already in
// `before`, or on a path in `changed` (in either direction). `shape::introduced`.
PVE.meta.Shape.introduced = (before, after, changed) =>
    PVE.meta.Core.call('findings_introduced', before || [], after || [], changed || []);

Object.assign(PVE.meta.Shape.prototype, {
    ask: function (fn, ...rest) {
        return PVE.meta.Core.call(fn, this.prefixes, this.tags, ...rest);
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

    // The prefix governing `path`, as its listing entry, or `null`.
    //
    // No production caller: the row builder asks `schemaIndex`, which the core has
    // already pruned by governance. It stays because its tests are what drive
    // `shape_governing` across the boundary, and most-specific-wins is a rule this
    // project has got wrong before -- a wrapper nobody calls is cheap, and losing
    // the only cross-boundary check of that rule is not.
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

    // Everything in `doc` that does not match what its governing schema says:
    // `[{ path, msg }]`, in the core's one order (by path). Type, enum and range
    // are decided by the core; a `format` comes back undecided, at its place in
    // that order, and is checked here with proxmoxlib's own validator for that
    // name -- the one thing the core leaves to whoever holds one. Resolved in
    // place, so the order is never sorted twice by two rules.
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

