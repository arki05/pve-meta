// ---------------------------------------------------------------------------
// EditSet: the staged edits on a document (`edit::EditSet`).
//
// The difference between the stored document and the planned one, as edits
// `{ path, op: 'set' | 'delete', value }`: `between` derives it, the tree renders
// `apply` -- the document as it would be -- and one Apply writes the planned
// subtree at `writeView`. A set is the server's `view::replace` and a delete its
// `view::remove`, so what the editor predicts and what a `PUT ?view=` does are
// the same function.
//
// The set owns its list (`edits`, read it; never push to it) and every operation
// on it is the core's. Nothing changes a set in place: the panel derives a new one
// whenever the planned document changes (`setPlanned`), so the set is never a log
// of what was done that could drift from what is shown.
// ---------------------------------------------------------------------------

PVE.meta.EditSet = function (edits) {
    this.edits = edits || [];
};

PVE.meta.EditSet.empty = () => new PVE.meta.EditSet([]);

// What would have to be staged to turn `stored` into `edited`, as row edits. A
// pure key reordering is not a change (key order is not a value, DESIGN §2) and
// stages nothing.
PVE.meta.EditSet.between = (stored, edited) =>
    new PVE.meta.EditSet(PVE.meta.Core.call('edits_between', stored, edited));

// Every path at which two documents differ in *value* -- what an edit is
// answerable for. A pure reordering changes none. `edit::changed_paths`.
PVE.meta.EditSet.changedPaths = (was, now) => PVE.meta.Core.call('changed_paths', was, now);

Object.defineProperty(PVE.meta.EditSet.prototype, 'length', {
    get: function () {
        return this.edits.length;
    },
});

Object.assign(PVE.meta.EditSet.prototype, {
    isEmpty: function () {
        return this.edits.length === 0;
    },

    // The document as it would be once these edits are applied to `stored`.
    apply: function (stored) {
        return PVE.meta.Core.call('edits_apply', stored || {}, this.edits);
    },

    // The narrowest view covering every staged path (`''` is the document root),
    // or `null` with nothing staged.
    writeView: function () {
        return PVE.meta.Core.call('edits_write_view', this.edits);
    },
});

