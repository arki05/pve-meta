// Offline smoke test of pve-meta-tree.js: the editor's helpers, the row builder,
// the staging model and the text-editor markers -- through the real pve-meta core,
// loaded from the same `.wasm` the package ships. A minimal Ext/PVE shim is enough;
// none of this touches the DOM.
//
// The rules themselves (the YAML codec, key names, coverage, shadowing, the edit
// set, the schema findings) are tested where they live, in Rust. What this suite
// shows is that the wasm build loads and answers, that the JavaScript objects over it
// (Codec, Access, Shape, EditSet) hand the right things in and out, and that the
// editor's own logic on top of them still does what it did.
//
// Build the core first: `make wasm` (or `cargo build -p pve-meta-wasm --target
// wasm32-unknown-unknown --profile wasm`). PVE_META_WASM overrides the path.
const fs = require('fs');
const vm = require('vm');
const path = require('path');

const WASM =
    process.env.PVE_META_WASM ||
    path.join(__dirname, '..', '..', 'target', 'wasm32-unknown-unknown', 'wasm', 'pve_meta_wasm.wasm');
if (!fs.existsSync(WASM)) {
    console.log('FAIL the core is not built: ' + WASM + '\n  run `make wasm` first');
    process.exit(1);
}

const ctx = {
    console,
    window: {},
    document: { createElement: () => ({}), head: { appendChild() {} } },
    Promise,
    WebAssembly,
    TextEncoder,
    TextDecoder,
    gettext: (s) => s,
    Ext: {
        // Just enough of the VTypes singleton for PVE.meta.Utils.checkFormat: the real
        // validators live in proxmoxlib, and the point of that helper is that it calls
        // whatever is registered rather than reimplementing it.
        form: {
            field: {
                VTypes: {
                    DnsName: (v) => /^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$/.test(v),
                    DnsNameText: 'not a valid dns-name',
                    IPAddress: (v) => /^(\d{1,3}\.){3}\d{1,3}$/.test(v),
                    IPAddressText: 'not a valid ipv4',
                },
            },
        },
        ns(path) {
            let cur = ctx;
            path.split('.').forEach((p) => {
                cur[p] = cur[p] || {};
                cur = cur[p];
            });
        },
        isArray: Array.isArray,
        encode: JSON.stringify,
        decode: JSON.parse,
        // Ext's real semantics, not Object.assign: the optional third argument is
        // *defaults*, applied before `config`, so config wins over it. A shim that
        // let the last argument win hid a guard that never guarded.
        apply(object, config, defaults) {
            if (defaults) {
                Object.assign(object, defaults);
            }
            return Object.assign(object, config || {});
        },
        emptyFn() {},
        htmlEncode: (s) => String(s),
        String: { format: (t, ...a) => t.replace(/\{(\d)\}/g, (m, i) => a[i]) },
        Object: { toQueryString: (o) => new URLSearchParams(o).toString() },
        define(name, cfg) {
            const parts = name.split('.');
            let cur = ctx;
            parts.slice(0, -1).forEach((p) => {
                cur[p] = cur[p] || {};
                cur = cur[p];
            });
            cur[parts[parts.length - 1]] = cfg;
            ctx.__defined.push(name);
        },
        data: { TreeModel: {} },
        Msg: { alert: (title, msg) => ctx.__alerts.push([title, msg]) },
        window: { Window: {} },
        panel: { Panel: {} },
        button: { Segmented: {} },
    },
    Proxmox: { Utils: { format_boolean: (v) => (v ? 'Yes' : 'No') } },
    __defined: [],
    __alerts: [],
};
ctx.PVE = {};
vm.createContext(ctx);
vm.runInContext(
    fs.readFileSync(path.join(__dirname, '..', 'pve-meta-tree.js'), 'utf8'),
    ctx,
    { filename: 'pve-meta-tree.js' },
);

let fails = 0;
const eq = (name, got, want) => {
    const g = JSON.stringify(got);
    const w = JSON.stringify(want);
    if (g !== w) {
        fails++;
        console.log(`FAIL ${name}\n  got  ${g}\n  want ${w}`);
    } else {
        console.log(`ok   ${name}`);
    }
};
const throws = (name, fn, contains) => {
    let err = null;
    try {
        fn();
    } catch (e) {
        err = e;
    }
    eq(name, !!err && (!contains || String(err.message).indexOf(contains) !== -1), true);
    return err;
};

console.log('--- before the core has loaded: everything that asks it fails closed ---');
// The core is lazy, and `syncButtons` runs on render ahead of the first document
// read; the registry grids' dialogs can open before it too. This editor has
// shipped two lazy-load ordering bugs already (js-yaml's). Nothing below may throw,
// and nothing may claim an edit is possible before the rule that decides it is here.
{
    const Core = ctx.PVE.meta.Core;
    const P0 = ctx.PVE.meta.TreePanel;
    eq('the core reports itself unloaded', Core.loaded(), false);
    throws('a direct call says so instead of trapping', () => Core.call('abi'), 'not loaded');
    // Full write access on paper, but nothing is editable until the core can judge
    // it: the guard fails CLOSED, never open.
    const early = { access: { read: 1, write: 1, scopes: [{ prefix: 'traefik', mode: 'rw' }] } };
    early.editableFor = P0.editableFor;
    eq('nothing is editable before the core arrives', early.editableFor('traefik.spec.host'), false);
    eq('... not even with full write access', early.editableFor(''), false);
    // The two name validators fail OPEN, deliberately: they only refuse early and in
    // words, and the server refuses the same names on its own. An unloaded core
    // means no early answer, not a field that cannot be typed into.
    eq('a key name is not refused before the core arrives', ctx.PVE.meta.Utils.keyPathError('bad key'), null);
    eq('nor a file name', ctx.PVE.meta.Utils.fileNameError('my file'), null);
}

console.log('\n--- the core loads and answers (the real .wasm, through the real glue) ---');
// What the browser does with `instantiateStreaming`, done synchronously here: the
// same bytes, the same four exports, the same `PVE.meta.Core.attach`.
const bytes = fs.readFileSync(WASM);
const t0 = process.hrtime.bigint();
const wasmModule = new WebAssembly.Module(bytes);
const instance = new WebAssembly.Instance(wasmModule, {});
const loadMs = Number(process.hrtime.bigint() - t0) / 1e6;
ctx.PVE.meta.Core.attach(instance);
const Core = ctx.PVE.meta.Core;
console.log(`     ${WASM}\n     ${bytes.length} bytes, compiled and instantiated in ${loadMs.toFixed(1)} ms`);
eq('the ABI version is the one the glue expects', Core.call('abi'), Core.ABI);
eq('and now the core reports itself loaded', Core.loaded(), true);
{
    // The same guards, the other way round, once the core is here.
    const late = { access: { read: 1, write: 1, scopes: [] } };
    late.editableFor = ctx.PVE.meta.TreePanel.editableFor;
    eq('editable once the core can judge it', late.editableFor('traefik.spec.host'), true);
    eq('and a bad key name is refused now', typeof ctx.PVE.meta.Utils.keyPathError('bad key'), 'string');
    eq('and a bad file name too', typeof ctx.PVE.meta.Utils.fileNameError('my file'), 'string');
    eq('... including one the API would refuse for its length', typeof ctx.PVE.meta.Utils.fileNameError('a'.repeat(129)), 'string');
    eq('... while 128 is fine', ctx.PVE.meta.Utils.fileNameError('a'.repeat(128)), null);
}
eq('a call goes through the linear-memory ABI and back', Core.call('parse', 'yaml', 'a: 1\nb: [x, y]\n'), { a: 1, b: ['x', 'y'] });
eq('non-ASCII survives both copies', Core.call('parse', 'yaml', 'k: ünïcøde 日本語 🚀\n'), { k: 'ünïcøde 日本語 🚀' });
{
    // A document larger than the initial output buffer, and than a wasm page: the
    // memory grows under the call and the glue must read `memory.buffer` afresh.
    const big = {};
    for (let i = 0; i < 20000; i++) {
        big['key' + i] = 'value ' + i + ' ' + 'x'.repeat(40);
    }
    const text = Core.call('dump', 'yaml', big);
    eq('a megabyte round trips through the buffer', text.length > 1000000 && JSON.stringify(Core.call('parse', 'yaml', text)) === JSON.stringify(big), true);
    // And a call after the growth still works (the output pointer moved).
    eq('the instance is fine afterwards', Core.call('covers', 'a', 'a.b'), true);
}
{
    const err = throws('a parse error is a CoreError, not a trap', () => Core.call('parse', 'yaml', 'a: 1\nb: [\n'), 'failed to parse');
    eq('... carrying the line the parser stopped on', err && err.line, 3);
    // (`instanceof Error` would be the vm context's own Error, not this realm's.)
    eq('... and it is a CoreError with a message', err instanceof ctx.PVE.meta.CoreError && typeof err.message === 'string', true);
    throws('an unknown function is an error', () => Core.call('no_such_function'), 'unknown function');
    throws('a bad argument is an error', () => Core.call('covers', 'a', 'a b'), 'invalid path');
    eq('the instance is fine after errors', Core.call('parse', 'yaml', 'ok: 1\n'), { ok: 1 });
}

console.log('\n--- classes defined ---');
eq('defined', ctx.__defined, [
    'PVE.meta.Footer',
    'PVE.meta.TreeModel',
    'PVE.meta.AddKeyWindow',
    'PVE.meta.AddRuleWindow',
    'PVE.meta.DeclareKeyWindow',
    'PVE.meta.EditValueWindow',
    'PVE.meta.TextWindow',
    'PVE.meta.TreePanel',
    'PVE.meta.DocumentWindow',
    'PVE.meta.NewRegistryWindow',
    'PVE.meta.ServiceToken',
    'PVE.meta.RegistryGrid',
    'PVE.meta.DatacenterPanel',
]);

const U = ctx.PVE.meta.Utils;
const Codec = ctx.PVE.meta.Codec;
const Access = ctx.PVE.meta.Access;
const Shape = ctx.PVE.meta.Shape;
const EditSet = ctx.PVE.meta.EditSet;
const Markers = ctx.PVE.meta.Markers;

// A panel reduced to its staging: one stored document, what is staged on it, and
// the methods that change what is staged -- no tree behind them, so a test can look
// at the edit set and the planned document directly.
const stagingStub = (stored, edits) => {
    const P = ctx.PVE.meta.TreePanel;
    const stub = { docId: '1', pending: new EditSet(edits || []), docState: { 1: { digest: 'd', data: stored } } };
    [
        'setPlanned', 'stage', 'stageFromView', 'plannedData', 'isDirty', 'revertPending',
        'discardRow', 'discardListMember', 'listAt', 'stageListMember',
    ].forEach((m) => (stub[m] = P[m]));
    stub.dataOf = (id) => (stub.docState[id] || {}).data || {};
    stub.buildTree = () => {};
    stub.syncButtons = () => {};
    return stub;
};

console.log('\n--- PVE.meta.compose: how the panel is three method sets sharing one `this` ---');
eq('merges left to right into a new object', ctx.PVE.meta.compose({ a: 1 }, { b: 2 }, { c: 3 }), { a: 1, b: 2, c: 3 });
const composeParts = [{ a: 1 }, { b: 2 }];
ctx.PVE.meta.compose(...composeParts);
eq('does not mutate its inputs', composeParts, [{ a: 1 }, { b: 2 }]);
let composeThrew = false;
try {
    ctx.PVE.meta.compose({ a: 1 }, { a: 2 });
} catch (err) {
    composeThrew = true;
}
eq('throws on a duplicate member instead of picking a winner', composeThrew, true);

console.log('\n--- Access: the coverage rule is the core\'s, read from a /meta/access answer ---');
// The rule itself (a scope on `p` covers `p`, `p__` and `p.*`, and nothing else)
// is tested in Rust; these show the face hands the right shapes across, Perl's
// `1`/`0` booleans included.
eq('a scope covers its subtree', Access.covers('traefik', 'traefik.spec.host'), true);
eq('and the sibling comment key -- the one comment-key rule', Access.covers('traefik', 'traefik__'), true);
eq('a longer name is not a child', Access.covers('traefik', 'traefikx'), false);
eq('never the root', Access.covers('traefik', ''), false);
eq('full write access', Access.hasAnyWrite({ write: 1, scopes: [] }), true);
eq('one rw scope', Access.hasAnyWrite({ write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] }), true);
eq('read-only scopes are not write access', Access.hasAnyWrite({ write: 0, scopes: [{ prefix: 'netbird', mode: 'ro' }] }), false);
eq('an auditor holds nothing', Access.hasAnyWrite({ read: 1, write: 0, scopes: [] }), false);
eq('a missing access object is not write access', Access.hasAnyWrite(undefined), false);
eq('canWrite inside an rw scope', Access.canWrite({ write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] }, 'traefik.spec'), true);
eq('canWrite outside it', Access.canWrite({ write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] }, 'netbird'), false);

eq('join root', U.joinPath('', 'a'), 'a');
eq('join nested', U.joinPath('a.b', 'c'), 'a.b.c');
eq('isComment', [U.isComment('k__'), U.isComment('__'), U.isComment('k')], [true, true, false]);

console.log('\n--- value helpers ---');
eq('kind array', U.kindOf(['a']), 'array');
eq('kind map', U.kindOf({}), 'map');
eq('display string', U.displayValue('a b', 'string'), 'a b');
eq('display array', U.displayValue(['a', 'b'], 'array'), '["a","b"]');
eq('parse number', U.parseValue('42', 'number'), 42);
eq('parse bool', U.parseValue('true', 'boolean'), true);
eq('parse array json', U.parseValue('["a","b"]', 'array'), ['a', 'b']);
eq('parse array csv', U.parseValue('a, b', 'array'), ['a', 'b']);
eq('parse array empty', U.parseValue('', 'array'), []);
eq('valueAt walks maps', U.valueAt({ a: { b: 1 } }, 'a.b'), 1);
eq('valueAt stops at a list', U.valueAt({ a: [1] }, 'a.0'), undefined);
eq('valueAt of the root', U.valueAt({ a: 1 }, ''), { a: 1 });

console.log('\n--- the row editor field comes from the grammar first ---');
eq('editor enum', U.editorFor({ kind: 'string', enumValues: ['a'] }).xtype, 'combobox');
eq('editor boolean', U.editorFor({ kind: 'boolean' }).xtype, 'proxmoxcheckbox');
eq('editor number', U.editorFor({ kind: 'number' }).xtype, 'numberfield');
eq('editor array', U.editorFor({ kind: 'array' }).xtype, 'textfield');

// A grammar's `minimum`/`maximum` reach the number editor, and its `format` is
// resolved to the proxmoxlib vtype that already validates that shape (DESIGN §3).
let numEd = U.editorFor({ kind: 'number', minimum: 1, maximum: 65535 });
eq('editor number honours minimum', numEd.minValue, 1);
eq('editor number honours maximum', numEd.maxValue, 65535);
eq('editor number without a range sets none', U.editorFor({ kind: 'number' }).minValue, undefined);
eq('a zero minimum is not dropped as falsy', U.editorFor({ kind: 'number', minimum: 0 }).minValue, 0);
eq('editor format -> vtype', U.editorFor({ kind: 'string', format: 'ipv4' }).vtype, 'IPAddress');
eq('editor format cidr', U.editorFor({ kind: 'string', format: 'CIDR' }).vtype, 'IP64CIDRAddress');
eq('an unknown format does not constrain the field',
    U.editorFor({ kind: 'string', format: 'no-such-format' }).vtype, undefined);
eq('no format, no vtype', U.editorFor({ kind: 'string' }).vtype, undefined);
eq('enum wins over format', U.editorFor({ kind: 'string', format: 'ipv4', enumValues: ['a'] }).xtype,
    'combobox');

console.log('\n--- selector text (the Access tooltip) ---');
eq('selector all', U.selectorText({ all: true }), 'all guests');
eq('selector tag', U.selectorText({ tag: 'traefik' }), 'tag: traefik');

console.log('\n--- row icons (DESIGN §12) ---');
eq('icons', ctx.PVE.meta.Icons, {
    map: 'fa fa-folder',
    mapExpanded: 'fa fa-folder-open',
    leaf: 'fa fa-file-text-o',
});

console.log('\n--- Codec: the store\'s YAML, in the browser ---');
// The exact canonical dump the live store produced for guest 200. There is no
// second emitter any more: this IS serde_yaml_ng, so reading what the server writes
// and writing what the server reads is the same code rather than a settled dispute.
const storeYaml =
    'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups__: asdf\n  groups:\n  - lan\n';
const storeDoc = {
    traefik: { spec: { host: 'ct200.example' } },
    netbird: { groups__: 'asdf', groups: ['lan'] },
};
eq('load the store dump', Codec.parse(storeYaml, 'yaml'), storeDoc);
eq('dump the store document -- the same bytes', Codec.dump(storeDoc, 'yaml'), storeYaml);
eq('round trip the store document', Codec.parse(Codec.dump(storeDoc, 'yaml'), 'yaml'), storeDoc);
eq('empty document is the empty map', Codec.parse('', 'yaml'), {});
eq('a null document is the empty map too', Codec.parse('~\n', 'yaml'), {});
// A repeated subtree must not come back as an anchor/alias.
const shared = { a: 1 };
eq('no anchors', Codec.dump({ x: shared, y: shared }, 'yaml').indexOf('&') === -1, true);
// A long scalar stays on one line: a folded line is a changed line in the diff.
eq('no folding', Codec.dump({ a: 'x'.repeat(300) }, 'yaml').split('\n').length, 2);
// Documents are ordered maps (DESIGN §2).
eq('key order preserved', Object.keys(Codec.parse(Codec.dump({ b: 1, a: 2 }, 'yaml'), 'yaml')), ['b', 'a']);
// The store's own safety rules, now in the editor too rather than left to the 400.
throws('anchors and aliases are refused', () => Codec.parse('a: &x 1\nb: *x\n', 'yaml'), 'anchor');
throws('explicit tags are refused', () => Codec.parse('a: !!str 1\n', 'yaml'), 'tag');
throws('complex keys are refused', () => Codec.parse('? [1, 2]\n: v\n', 'yaml'), 'complex');
eq('JSON in', Codec.parse('{"a": [1, {"b": true}]}', 'json'), { a: [1, { b: true }] });
eq('JSON out is two-space pretty', Codec.dump({ a: [1] }, 'json'), '{\n  "a": [\n    1\n  ]\n}\n');
throws('a JSON error carries its line too', () => Codec.parse('{"a": 1,\n}', 'json'), 'parse').line === 2 || fails++;

console.log('\n--- the footer keeps the word its caller chose ---');
{
    // `sync` rewrites the primary button's text on every keystroke. Setting the word
    // only where the button is built left it to be overwritten by the next thing the
    // user typed -- so a modal's OK silently became the panel's Apply.
    const F = ctx.PVE.meta.Footer;
    const btn = (itemId, cfg) => {
        const b = Object.assign({ itemId: itemId, text: '', disabled: false }, cfg);
        b.setText = (t) => (b.text = t);
        b.setDisabled = (d) => (b.disabled = d);
        b.setIconCls = () => {};
        return b;
    };
    const owner = (b) => ({ down: (sel) => (sel === '#metaApply' ? b : null) });

    const ok = btn('metaApply', { metaApplyText: 'OK' });
    F.sync(owner(ok), { canApply: true, count: 0 });
    eq('a modal keeps OK through a sync', ok.text, 'OK');

    const apply = btn('metaApply', { metaApplyText: 'Apply' });
    F.sync(owner(apply), { canApply: true, count: 3 });
    eq('the panel still counts its staged edits', apply.text, 'Apply (3)');

    const bare = btn('metaApply', {});
    F.sync(owner(bare), { canApply: true, count: 0 });
    eq('a caller that named nothing gets Apply', bare.text, 'Apply');
}

console.log('\n--- Edit selection as text stages, like every other modal ---');
{
    const stored = { traefik: { spec: { host: 'a', port: 80 } }, netbird: { groups: ['lan'] } };

    // A subtree buffer is one edit at the view's own path, and it says everything
    // about what is inside that view -- a buffer for `traefik` replaces `traefik.spec`
    // whatever was staged there before. What is staged afterwards is the difference
    // that leaves, not the buffer.
    const stub = stagingStub(stored, [{ path: 'traefik.spec.host', op: 'set', value: 'b' }]);
    eq('an inner edit stages', stub.pending.length, 1);
    stub.stageFromView('traefik', { spec: { host: 'c' } });
    eq('the planned document is what was typed', stub.plannedData().traefik, { spec: { host: 'c' } });
    eq('... leaving everything outside the view alone', stub.plannedData().netbird, { groups: ['lan'] });
    eq('and the set is the difference, not the buffer',
        stub.pending.edits.map((e) => e.path + ':' + e.op).sort(),
        ['traefik.spec.host:set', 'traefik.spec.port:delete']);

    // The whole-document view: the empty path is the document, not a key named "".
    stub.stageFromView('', { only: 1 });
    eq('the root view stages as the document', stub.plannedData(), { only: 1 });
}

console.log('\n--- leaving Text only asks when the tree cannot carry the change ---');
{
    // Ordinary edits become staged rows, so switching needs no dialog and gets none.
    // A reorder or a reindent parses to the document it started as (decision 007:
    // order is kept, not meaning), so it stages nothing -- and switching would drop
    // it with no row to mark. That single case asks.
    const stored = { b: 1, a: 2 };
    const E = ctx.PVE.meta.EditSet;
    eq('a reordering stages nothing', E.between(stored, { a: 2, b: 1 }).isEmpty(), true);
    eq('a real edit still stages', E.between(stored, { b: 9, a: 2 }).length, 1);
    // The condition the dialog fires on: nothing staged, but the text did change.
    const asks = (set, textDirty) => set.isEmpty() && textDirty;
    eq('reorder: nothing staged, text dirty -> ask', asks(E.between(stored, { a: 2, b: 1 }), true), true);
    eq('a real edit -> no dialog', asks(E.between(stored, { b: 9, a: 2 }), true), false);
    eq('untouched buffer -> no dialog', asks(E.empty(), false), false);
}

console.log('\n--- request: the destroyed-component guard around API2Request ---');
{
    const sent = [];
    ctx.Proxmox.Utils.API2Request = (req) => sent.push(req);
    const owner = { isDestroyed: false };
    let calls = [];
    ctx.PVE.meta.request(owner, { url: '/x', params: { a: 1 }, success: (r) => calls.push(['ok', r]), failure: (r) => calls.push(['fail', r]) });
    const req = sent.pop();
    eq('method defaults to GET and the request is passed through', [req.method, req.url, req.params], ['GET', '/x', { a: 1 }]);
    req.success('r1');
    req.failure('r2');
    eq('callbacks reach the owner while it lives', calls, [['ok', 'r1'], ['fail', 'r2']]);
    owner.isDestroyed = true;
    calls = [];
    req.success('r3');
    req.failure('r4');
    eq('... and are dropped once it is destroyed -- the guard is what API2Request got, not the originals', calls, []);
    ctx.PVE.meta.request({ isDestroyed: false }, { url: '/y', method: 'PUT' });
    eq('an explicit method wins over the default', sent.pop().method, 'PUT');
    eq('no failure handler stays no handler', sent.length === 0 && ctx.PVE.meta.request({}, { url: '/z' }) === undefined && sent.pop().failure, undefined);
    delete ctx.Proxmox.Utils.API2Request;
}

console.log('\n--- enforce: an enforcing prefix\'s findings say so ---');
{
    const S = { type: 'object', properties: { port: { type: 'integer' } } };
    const strict = new Shape([{ prefix: 't', selector: { all: 1 }, enforce: 1, schema: S }], []);
    const lax = new Shape([{ prefix: 't', selector: { all: 1 }, schema: S }], []);
    eq('an enforcing prefix flags its findings', strict.findings({ t: { port: 'x' } }), [{ path: 't.port', msg: 'expected integer', enforced: true }]);
    eq('an ordinary one does not', lax.findings({ t: { port: 'x' } }), [{ path: 't.port', msg: 'expected integer' }]);
    eq('Perl\'s 0 is not enforce', new Shape([{ prefix: 't', selector: { all: 1 }, enforce: 0, schema: S }], []).findings({ t: { port: 'x' } })[0].enforced, undefined);
    eq('the banner line says which', U.findingText({ path: 't.port', msg: 'expected integer', enforced: true }), 'enforced: t.port: expected integer');
    eq('... and stays plain otherwise', U.findingText({ path: 't.port', msg: 'expected integer' }), 't.port: expected integer');
}

console.log('\n--- Buffer: what both text editors do to a Monaco buffer ---');
// The Text card and the subtree window share one copy of Format, the YAML | JSON
// switch, Diff and "did anything change", driven through a fake editor: the rules
// they call are the core's; what is checked is the choreography around them.
{
    const Buffer = ctx.PVE.meta.Buffer;
    const editor = (value) => ({
        value,
        sets: 0,
        getValue() { return this.value; },
        setValue(v) { this.value = v; this.sets++; },
        getModel() { return 'the-model'; },
    });
    const alerts = () => ctx.__alerts.splice(0);
    const langsSet = [];
    ctx.window.monaco = { editor: { setModelLanguage: (model, lang) => langsSet.push([model, lang]) } };
    const diffs = [];
    ctx.PVE.meta.Monaco.confirmDiff = (cfg) => diffs.push(cfg);
    const btn = { value: null, suspended: 0, suspendEvents() { this.suspended++; }, resumeEvents() { this.suspended--; }, setValue(v) { this.value = v; } };

    // A file as somebody wrote it: valid YAML in a layout no emitter would choose.
    const handWritten = 'b:   1\na: [x, y]\n';
    const canonical = Codec.dump(Codec.parse(handWritten, 'yaml'), 'yaml');
    eq('the hand-written layout is not the canonical one', canonical !== handWritten, true);

    // baseline
    eq('baseline in YAML is the loaded text itself', Buffer.baseline({ editor: editor(''), lang: 'yaml', original: handWritten }), { lang: 'yaml', text: handWritten });
    eq('baseline in JSON is the loaded text as JSON', Buffer.baseline({ editor: editor(''), lang: 'json', original: handWritten }), { lang: 'json', text: Codec.dump({ b: 1, a: ['x', 'y'] }, 'json') });
    eq('a loaded text that is not YAML falls back to a YAML baseline', Buffer.baseline({ editor: editor(''), lang: 'json', original: 'a: [\n' }), { lang: 'yaml', text: 'a: [\n' });

    // unchanged
    eq('unchanged: the loaded text, untouched', Buffer.unchanged({ editor: editor(handWritten), lang: 'yaml', original: handWritten }), true);
    // Silently, both ways. This used to raise "No changes." from inside the
    // predicate, which is how a dialog appeared in editors that never asked for
    // one: answering a question and interrupting the user are two jobs, and only
    // one of them was in the name.
    eq('... and says nothing', alerts(), []);
    eq('unchanged: an edit', Buffer.unchanged({ editor: editor('b: 2\n'), lang: 'yaml', original: handWritten }), false);
    eq('... silently', alerts(), []);
    eq('unchanged: the same document toggled to JSON is still unchanged', Buffer.unchanged({ editor: editor(Codec.dump({ b: 1, a: ['x', 'y'] }, 'json')), lang: 'json', original: handWritten }), true);
    alerts();

    // format
    let ed = editor(handWritten);
    eq('format re-dumps canonically', Buffer.format({ editor: ed, lang: 'yaml', original: handWritten }), true);
    eq('... to the store\'s own layout', ed.value, canonical);
    eq('format of a canonical buffer changes nothing', Buffer.format({ editor: ed, lang: 'yaml', original: handWritten }), false);
    eq('... and does not touch the editor', ed.sets, 1);
    ed = editor('a: [\n');
    eq('format refuses a buffer that does not parse', Buffer.format({ editor: ed, lang: 'yaml', original: '' }), false);
    eq('... leaves it alone', [ed.value, ed.sets], ['a: [\n', 0]);
    eq('... and says why', alerts().map((a) => a[0]), ['Cannot format']);

    // convert + render: the presentation toggle, there and back
    ed = editor(handWritten);
    const buf = { editor: ed, lang: 'yaml', original: handWritten };
    const value = Buffer.convert(buf, 'json', btn);
    eq('convert parses the buffer in its current language', value, { b: 1, a: ['x', 'y'] });
    buf.lang = 'json';
    Buffer.render(buf, value);
    eq('render switches the model language', langsSet, [['the-model', 'json']]);
    eq('... and shows the value as JSON', ed.value, Codec.dump(value, 'json'));
    const back = Buffer.convert(buf, 'yaml', btn);
    buf.lang = 'yaml';
    Buffer.render(buf, back);
    eq('switching back to YAML restores the hand-written text, not a re-dump', ed.value, handWritten);
    eq('... (the regression the two copies had between them)', ed.value !== canonical, true);
    ed.value = 'b: 2\n';
    Buffer.render(buf, Buffer.convert(buf, 'json', btn));
    eq('an edited document is re-dumped, since the loaded text no longer matches', ed.value, 'b: 2\n');
    eq('no alert and the toggle was never reset', [alerts(), btn.value, btn.suspended], [[], null, 0]);
    ed = editor('a: [\n');
    eq('convert refuses a buffer that does not parse', Buffer.convert({ editor: ed, lang: 'yaml', original: '' }, 'json', btn), undefined);
    eq('... puts the toggle back on the current language, events suspended around it', [btn.value, btn.suspended], ['yaml', 0]);
    eq('... names the target language', alerts().map((a) => a[1].indexOf('Cannot convert to JSON') === 0), [true]);
    eq('... and the buffer is untouched', ed.sets, 0);

    // diff
    Buffer.diff({ editor: editor('b: 2\n'), lang: 'yaml', original: handWritten }, 'the title');
    eq('diff shows the buffer against the baseline', diffs.pop(), { title: 'the title', original: handWritten, modified: 'b: 2\n', lang: 'yaml' });
    Buffer.diff({ editor: editor('{}'), lang: 'json', original: 'a: [\n' }, 't');
    eq('... falling back to YAML when the loaded text cannot be shown as JSON', diffs.pop().lang, 'yaml');
    delete ctx.window.monaco;
}

console.log('\n--- YAML property test: parse(dump(x)) deep-equals x ---');
// A seeded PRNG, so a failure names a document that can be reproduced exactly.
const rng = (seed) =>
    function () {
        seed |= 0;
        seed = (seed + 0x6d2b79f5) | 0;
        let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };

// Keys and scalars that have historically broken hand-written YAML: structural
// punctuation, quotes, comment markers, unicode, and strings that look like
// numbers, booleans or nulls and must come back as strings.
const KEYS = [
    'plain',
    'has:colon',
    'colon: space',
    "quo'te",
    'dq"uote',
    '#hash',
    'trailing #hash',
    'dash-key',
    '-',
    '? question',
    '[bracket]',
    '{brace}',
    'a,b',
    '&anchor',
    '*alias',
    '|pipe',
    '>fold',
    '%percent',
    '@at',
    '`tick',
    '!bang',
    'ünïcøde',
    '日本語',
    'Ελληνικά',
    '🚀',
    '007',
    '1.5e3',
    '0x10',
    '-12',
    'true',
    'False',
    'yes',
    'no',
    'on',
    'off',
    'null',
    '~',
    '',
    ' leading',
    'trailing ',
    'inner  spaces',
    'multi\nline',
    'tab\there',
    'documented__',
    '__',
    'constructor',
    'hasOwnProperty',
    'toString',
];
const SCALARS = [
    '',
    'plain',
    'has: colon',
    'ends with #',
    "it's",
    'say "hi"',
    '- not a list',
    '007',
    '1e5',
    '0b101',
    'true',
    'null',
    '~',
    'ünïcøde é',
    '日本語のテキスト',
    '🚀 launched',
    'multi\nline\ntext',
    'tab\tseparated',
    ' padded ',
    'x'.repeat(200),
    0,
    1,
    -1,
    42,
    -0.25,
    3.5,
    1234567890,
    true,
    false,
];

const pick = (rnd, arr) => arr[Math.floor(rnd() * arr.length) % arr.length];

const genValue = (rnd, depth) => {
    const r = rnd();
    if (depth <= 0 || r < 0.55) {
        return pick(rnd, SCALARS);
    } else if (r < 0.8) {
        return genMap(rnd, depth - 1);
    }
    return genArray(rnd, depth - 1);
};

const genArray = (rnd, depth) => {
    const n = Math.floor(rnd() * 4);
    const out = [];
    for (let i = 0; i < n; i++) {
        out.push(genValue(rnd, depth));
    }
    return out;
};

function genMap(rnd, depth) {
    const n = Math.floor(rnd() * 5);
    const out = {};
    for (let i = 0; i < n; i++) {
        // Not via a literal: `__proto__` as a key would be swallowed by the
        // prototype setter rather than becoming a document key.
        Object.defineProperty(out, pick(rnd, KEYS), {
            value: genValue(rnd, depth),
            enumerable: true,
            writable: true,
            configurable: true,
        });
    }
    return out;
}

// A fixed corpus first, so the hostile shapes are exercised on every run and not
// only when the generator happens to reach for them.
const CORPUS = [
    { '': 'empty key' },
    { 'has:colon': 'a', 'colon: space': 'b' },
    { "quo'te": "it's", 'dq"uote': 'say "hi"' },
    { '#hash': 'ends with #', 'trailing #hash': '- not a list' },
    { 'ünïcøde': 'é', 日本語: '値', '🚀': '🚀 launched' },
    { '007': '007', '1.5e3': '1.5e3', '0x10': '0x10', '-12': '-12' },
    { true: 'true', no: 'off', null: '~', '~': 'null' },
    { 'multi\nline': 'c\nd', 'tab\there': ' padded ' },
    { documented__: 'a note', __: 'about the map', constructor: 'x' },
    { a: { b: [{ c: [1, 2, true, 'x', ''] }] }, e: {}, l: [] },
    { n: -0.25, z: 0, big: 1234567890, long: 'x'.repeat(300) },
];

let propFails = 0;
for (let i = 0; i < 500 + CORPUS.length; i++) {
    const rnd = rng(0x5eed + i);
    // Regenerating an empty map wastes a case, and rnd() has already advanced.
    let doc = i < CORPUS.length ? CORPUS[i] : genMap(rnd, 3);
    while (!Object.keys(doc).length) {
        doc = genMap(rnd, 3);
    }
    let text;
    let back;
    try {
        text = Codec.dump(doc, 'yaml');
        back = Codec.parse(text, 'yaml');
    } catch (err) {
        propFails++;
        if (propFails <= 3) {
            console.log(`FAIL seed ${i}: ${err.message}\n  doc  ${JSON.stringify(doc)}`);
        }
        continue;
    }
    // JSON.stringify compares structure, values *and* key order: the codec keeps
    // the order it was given, even though order is not a value.
    if (JSON.stringify(back) !== JSON.stringify(doc)) {
        propFails++;
        if (propFails <= 3) {
            console.log(
                `FAIL seed ${i}\n  doc  ${JSON.stringify(doc)}\n  yaml ${JSON.stringify(text)}\n  back ${JSON.stringify(back)}`,
            );
        }
    }
}
eq('the corpus and 500 generated documents round trip', propFails, 0);

console.log('\n--- row merge: document + shape ---');
const P = ctx.PVE.meta.TreePanel;
const TRAEFIK_SCHEMA = {
    type: 'object',
    properties: {
        spec: {
            type: 'object',
            properties: {
                host: { type: 'string', description: 'Public host name' },
                port: { type: 'integer', default: 80 },
                scheme: { type: 'string', enum: ['http', 'https'] },
            },
        },
    },
};
const panel = {
    registryDoc: false,
    docId: '200',
    tags: ['traefik'],
    access: { read: 1, write: 1, scopes: [] },
    // Two lists now, two rules (DESIGN sections 3-4): prefixes decide shape,
    // permissions decide access.
    prefixes: [
        { prefix: 'traefik', selector: { tag: 'traefik' }, schema: TRAEFIK_SCHEMA },
        { prefix: 'netbird', selector: { all: true } },
    ],
    permissions: [
        {
            name: 'traefik',
            authid: 'svc@pve!traefik',
            rules: [{ prefix: 'traefik', mode: 'rw', selector: { tag: 'traefik' } }],
        },
        {
            name: 'netbird',
            authid: 'svc@pve!netbird',
            rules: [{ prefix: 'netbird', mode: 'ro', selector: { all: true } }],
        },
    ],
};
[
    'entry',
    'addData',
    'addShape',
    'schemaKind',
    'shapeFor', 'shapeInputs', 'buildShape',
    'docKind',
    'applicablePermissions',
    'accessFor',
    'accessSummary',
    'editableFor',
].forEach((m) => (panel[m] = P[m]));

const shape = panel.shapeFor('200');
// Most-specific first, then by name: the order the server lists in, and the one
// the Shape resolves in, whatever order the listing arrived in.
eq('the prefixes that reach this guest', shape.declared().map((n) => n.prefix), ['netbird', 'traefik']);
const scopes = panel.applicablePermissions.call(panel);
eq('the rules that reach it', scopes.map((s) => s.prefix), ['traefik', 'netbird']);
eq('... each carrying its file', scopes.map((s) => s.name + '/' + s.authid), ['traefik/svc@pve!traefik', 'netbird/svc@pve!netbird']);

const root = { key: '', path: '', children: {}, present: true, kind: 'map' };
panel.addData.call(panel, root, storeDoc);
panel.addShape.call(panel, root, shape);

const spec = root.children.traefik.children.spec.children;
eq('the shape adds unset rows', Object.keys(spec).sort(), ['host', 'port', 'scheme']);
eq('present key stays present', spec.host.present, true);
eq('declared key is unset', spec.port.present, false);
eq('display boolean', U.displayValue(1, 'boolean'), 'Yes');
eq('declared default carried', spec.port.defaultValue, 80);
eq('declared enum carried', spec.scheme.enumValues, ['http', 'https']);
// The grammar's description is the tooltip, not the Description column (DESIGN §12).
eq('grammar description is its own field', spec.host.grammarDescription, 'Public host name');
eq('grammar description is not the comment', spec.host.description, undefined);
eq('schema kind integer', panel.schemaKind({ type: 'integer' }), 'number');

// A declared type wins over the type inferred from the stored value.
root.children.traefik.children.spec.children.host.kind = 'number';
panel.addShape.call(panel, root, shape);
eq('grammar type wins', root.children.traefik.children.spec.children.host.kind, 'string');

// Comment key becomes the sibling's Description, never a row of its own.
eq('comment not a row', Object.keys(root.children.netbird.children).sort(), ['groups']);
eq('comment is the description', root.children.netbird.children.groups.description, 'asdf');
eq('array stays one leaf', root.children.netbird.children.groups.kind, 'array');

console.log('\n--- Set to Default answers "what should this be", not only "what if unset" ---');
{
    const staged = [];
    const sd = { stage: (path, op, value) => staged.push({ path, op, value }) };
    sd.setToDefault = P.setToDefault;
    const row = (data) => ({ data: Object.assign({ docId: '100', path: 'traefik.spec.port' }, data) });

    sd.setToDefault.call(sd, row({ present: false, defaultValue: 80 }));
    eq('an unset row still takes the default', staged.pop(), {
        path: 'traefik.spec.port', op: 'set', value: 80,
    });

    sd.setToDefault.call(sd, row({ present: true, rawValue: 8080, defaultValue: 80 }));
    eq('a wrong value can be put back to the default', staged.pop(), {
        path: 'traefik.spec.port', op: 'set', value: 80,
    });

    sd.setToDefault.call(sd, row({ present: true, rawValue: 80, defaultValue: 80 }));
    eq('a row already at its default stages nothing', staged.length, 0);

    sd.setToDefault.call(sd, row({ present: false }));
    eq('a row with no default stages nothing', staged.length, 0);

    // Structural values compare by content, not identity, and a map in another key
    // order is the same value.
    sd.setToDefault.call(sd, row({ present: true, rawValue: ['a'], defaultValue: ['a'] }));
    eq('an equal list is already at its default', staged.length, 0);
    sd.setToDefault.call(sd, row({ present: true, rawValue: { b: 1, a: 1 }, defaultValue: { a: 1, b: 1 } }));
    eq('a map in another key order is the same value', staged.length, 0);
}

console.log('\n--- the hostile document: the editor writes exactly what the store writes ---');
{
    // This is the end-to-end check that the editor's emitter is the one the store
    // uses: the document of values that break hand-written YAML, and the exact
    // bytes the store writes for it, through the wasm. The Rust suite pins the
    // same bytes natively (tests/formats.rs).
    const y = JSON.parse(
        fs.readFileSync(path.join(__dirname, '..', '..', 'testdata', 'yaml-cases.json'), 'utf8'),
    );
    eq('the fixture is present', Object.keys(y.document).length >= 40, true);
    const dumped = Codec.dump(y.document, 'yaml');
    if (dumped !== y.canonical) {
        const a = y.canonical.split('\n');
        const b = dumped.split('\n');
        for (let i = 0; i < Math.max(a.length, b.length); i++) {
            if (a[i] !== b[i]) {
                console.log('FAIL yaml dump differs from the store at line ' + (i + 1));
                console.log('  store  ' + JSON.stringify(a[i]));
                console.log('  editor ' + JSON.stringify(b[i]));
                fails++;
                break;
            }
        }
    } else {
        console.log('ok   the editor dumps the hostile document exactly as the store writes it');
    }
    eq('and reads the store\'s canonical text back exactly', Codec.parse(y.canonical, 'yaml'), y.document);
    eq('a JSON round trip changes nothing', Codec.parse(Codec.dump(JSON.parse(JSON.stringify(y.document)), 'yaml'), 'yaml'), y.document);
}

console.log('\n--- the document is read as YAML: the file\'s own order survives a staged edit ---');
{
    // `GET ...?format=json` renders the document as a native Perl hash, which has
    // no order and no booleans. Order is not a value, but an Apply at the root view
    // writes `plannedData()` back, and keeping the file's order is what stops that
    // from churning it.
    const text = 'zebra: 1\nalpha: 2\nmiddle:\n  z: 1\n  a: 2\n';
    eq('parse keeps the document order', Object.keys(Codec.parse(text, 'yaml')), ['zebra', 'alpha', 'middle']);
    eq('... at every level', Object.keys(Codec.parse(text, 'yaml').middle), ['z', 'a']);

    // And the order survives the trip the editor actually makes: parse, stage an
    // edit, dump. This is the property that keeps an Apply from churning the file.
    const planned = new EditSet([{ path: 'alpha', op: 'set', value: 9 }]).apply(Codec.parse(text, 'yaml'));
    eq('order survives a staged edit', Object.keys(planned), ['zebra', 'alpha', 'middle']);
    eq('... and the dump preserves it', Codec.dump(planned, 'yaml').indexOf('zebra') === 0, true);
}

console.log('\n--- a registry file that did not load is still a row ---');
{
    const G = ctx.PVE.meta.RegistryGrid.statics;
    // A prefix that stops parsing must still be listed, with its error --
    // silently ceasing to exist is the one failure mode with no symptom
    // (docs/decisions/003-registry-files-are-documents.md).
    const pfx = G.rowsFrom('prefixes', [
        { prefix: 'good', selector: { all: true }, origin: 'cluster' },
        { prefix: 'broken', origin: 'packaged', error: 'mapping values are not allowed here' },
    ]);
    eq('both files are rows', pfx.map((r) => r.name), ['good', 'broken']);
    eq('the failure keeps its error', pfx[1].error, 'mapping values are not allowed here');
    eq('... and says so where it would say what it does', pfx[1].description, pfx[1].error);
    eq('... and is still addressable, which is how it gets repaired', pfx[1].id, 'prefixes/broken');
    eq('... and keeps the origin, which is where to look for it', pfx[1].origin, 'packaged');
    eq('a file that loaded carries no error', pfx[0].error, undefined);

    const perm = G.rowsFrom('permissions', [
        { name: 'ops', authid: 'a@pve!t', rules: [], origin: 'cluster' },
        { name: 'bad', origin: 'cluster', error: 'missing field `authid`' },
    ]);
    eq('permissions the same way', perm.map((r) => r.name), ['ops', 'bad']);
    eq('a failed permission claims no authid', perm[1].authid, '');
    eq('... and no rules', perm[1].summary, '');

    // The safety half: a file that did not load must never describe anything, and
    // never grant anything. The listing carries it; the Shape and the rules drop it.
    const p2 = Object.assign({}, panel, {
        prefixes: [
            { prefix: 'netbird', selector: { all: true } },
            { prefix: 'broken', selector: { all: true }, error: 'nope' },
        ],
        permissions: [
            { name: 'ok', authid: 'a@pve', rules: [{ prefix: 'netbird', mode: 'rw', selector: { all: true } }] },
            { name: 'bad', authid: 'b@pve', error: 'nope', rules: [{ prefix: 'netbird', mode: 'rw', selector: { all: true } }] },
        ],
    });
    eq('a failed prefix reaches no guest, even carrying a selector', p2.shapeFor('200').declared().map((n) => n.prefix), ['netbird']);
    eq('a failed permission file grants nothing, even carrying rules', p2.applicablePermissions().map((r) => r.name), ['ok']);
}

console.log('\n--- a hidden declaration decorates a row, it never creates one ---');
{
    // A vocabulary the size of Traefik's is mostly keys nobody sets on a given
    // guest, and every one of them as a greyed row buries what the guest says.
    // `hidden` suppresses the ghost. It must not suppress data: a hidden key that
    // IS set still gets its row, its type and its default.
    const d = Object.assign({}, panel, {
        docId: '100',
        pending: ctx.PVE.meta.EditSet.empty(),
        docState: { 100: { digest: 'x', data: { t: { routers: { rule: 'Host(`a`)' } } } } },
        prefixes: [
            {
                prefix: 't',
                selector: { all: true },
                schema: {
                    type: 'object',
                    properties: {
                        host: { type: 'string' },
                        routers: {
                            type: 'object',
                            hidden: true,
                            properties: {
                                rule: { type: 'string' },
                                entrypoint: { type: 'string', hidden: false, default: 'web' },
                                timeout: { type: 'integer' },
                            },
                        },
                    },
                },
            },
        ],
    });
    ['documentEntries', 'plannedData', 'dataOf', 'shapeFor', 'docKind', 'entry', 'addData',
     'addShape', 'existing', 'applicablePrefixes', 'schemaKind'].forEach((m) => (d[m] = P[m]));
    const root = d.documentEntries.call(d);
    const routers = root.children.t.children.routers;

    eq('a shown declaration still gets its ghost row', root.children.t.children.host.present, false);
    eq('a hidden subtree is still a row, because the document has one', !!routers, true);
    eq('a hidden key that is set keeps its row', routers.children.rule.present, true);
    eq('... and is still typed by its schema', routers.children.rule.kind, 'string');
    eq('a hidden key that is not set has no row', routers.children.timeout, undefined);
    eq('an explicitly shown key inside a hidden subtree is offered',
        [!!routers.children.entrypoint, routers.children.entrypoint.present], [true, false]);
    eq('... with its default', routers.children.entrypoint.defaultValue, 'web');
}

console.log('\n--- a prefix is a declaration, with or without a schema ---');
{
    // A prefix is itself a statement about the document: something of mine
    // lives at this key. That is the statement permissions are written in terms of,
    // so it earns a row; it just has less to say than a schema'd one.
    const doc = (data) => {
        const d = Object.assign({}, panel, {
            docId: '100',
            pending: EditSet.empty(),
            docState: { 100: { digest: 'x', data: data } },
        });
        ['documentEntries', 'plannedData', 'dataOf', 'shapeFor', 'shapeInputs', 'buildShape', 'docKind',
         'entry', 'addData', 'addShape'].forEach((m) => (d[m] = P[m]));
        return d.documentEntries.call(d);
    };

    const empty = doc({});
    eq(
        'both prefixes get a row on an empty document',
        Object.keys(empty.children).sort(),
        ['netbird', 'traefik'],
    );
    eq('the schema-less one is unset, not missing', empty.children.netbird.present, false);
    eq('... and is a map, which is what Add goes into', empty.children.netbird.kind, 'map');
    eq('the schema-less prefix declares no children', Object.keys(empty.children.netbird.children), []);
    eq('the schema\'d one still paints its declared rows',
        Object.keys(empty.children.traefik.children.spec.children).sort(),
        ['host', 'port', 'scheme']);

    // And a prefix may simply hold a value. It is a key like any other, and one that
    // needs to say nothing but `true` should not have to grow a subkey to say it:
    // the stored value's own kind wins over the map the absent row falls back to.
    const scalar = doc({ netbird: true });
    eq('a prefix may hold a single scalar', scalar.children.netbird.kind, 'boolean');
    eq('... and it is present', scalar.children.netbird.present, true);
    eq('... and editable as the scalar it is', U.editorKind(scalar.children.netbird), 'inline');
}

console.log('\n--- Access: every rule whose prefix covers the row ---');
eq('access of a grammar row', panel.accessFor.call(panel, 'traefik.spec.port', scopes), [
    { name: 'traefik', mode: 'rw', selector: 'tag: traefik', prefix: 'traefik' },
]);
eq('access ro marked', panel.accessSummary(panel.accessFor.call(panel, 'netbird.groups', scopes)), 'netbird (ro)');
eq('access of an unclaimed row', panel.accessFor.call(panel, 'mine.key', scopes), []);
// Several principals may cover the same subtree; rw sorts before ro.
const overlapping = scopes.concat([
    { name: 'audit', authid: 'svc@pve!audit', prefix: 'traefik', mode: 'ro', selector: { all: true } },
]);
eq(
    'rw first, then ro',
    panel.accessSummary(panel.accessFor.call(panel, 'traefik.spec.host', overlapping)),
    'traefik, audit (ro)',
);

panel.access = { read: 1, write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] };
eq('scoped write inside', panel.editableFor.call(panel, 'traefik.spec.host'), true);
eq('scoped write outside', panel.editableFor.call(panel, 'netbird.groups'), false);
eq('scoped write on the comment key of the prefix', panel.editableFor.call(panel, 'traefik__'), true);

console.log('\n--- S6: a guest that does not carry the tag ---');
// A tag selector resolves against this guest's tags and nothing else. Holding a
// `traefik` rw scope of our own must not drag another principal's tag-selected
// rule onto a guest that is not tagged `traefik` -- the Access column would
// then name a writer who cannot in fact write here.
const untagged = Object.assign({}, panel);
untagged.tags = [];
untagged.access = { read: 1, write: 1, scopes: [{ prefix: 'traefik', mode: 'rw' }] };
const untaggedScopes = panel.applicablePermissions.call(untagged);
eq('a tag rule needs the tag', untaggedScopes.map((s) => s.prefix), ['netbird']);
eq(
    'no Access row for the prefix whose selector missed',
    panel.accessFor.call(untagged, 'traefik.spec.host', untaggedScopes),
    [],
);
// Same rule on the shape side: no tag, no declared rows from that prefix.
eq(
    'prefix applicability follows the same tags',
    untagged.shapeFor('200').declared().map((n) => n.prefix),
    ['netbird'],
);

console.log('\n--- schema findings for the text editor ---');
// Prefix objects, the same shape GET /meta/prefixes returns.
const GRAMMAR = Shape.of([
    {
        prefix: 'traefik',
        selector: { all: true },
        schema: {
            type: 'object',
            properties: {
                spec: {
                    type: 'object',
                    properties: {
                        host: { type: 'string', format: 'dns-name', description: 'Public host name' },
                        port: { type: 'integer', minimum: 1, maximum: 65535, default: 80 },
                        scheme: { type: 'string', enum: ['http', 'https'] },
                        enabled: { type: 'boolean' },
                    },
                },
            },
        },
    },
], []);

eq('a clean document has no findings',
    GRAMMAR.findings({ traefik: { spec: { host: 'a.example', port: 80, scheme: 'https', enabled: true } } }),
    []);

eq('each rule is reported at its own path',
    GRAMMAR.findings({ traefik: { spec: { port: 70000, scheme: 'ftp', enabled: 'yes' } } })
        .map((f) => f.path),
    ['traefik.spec.enabled', 'traefik.spec.port', 'traefik.spec.scheme']);

// A `format` is the one thing the core hands back: it is checked with proxmoxlib's
// own validator for that name, and lands among the findings like any other.
eq('a format is checked by the vtype and lands in the findings',
    GRAMMAR.findings({ traefik: { spec: { host: 'not a host name!' } } }),
    [{ path: 'traefik.spec.host', msg: 'not a valid dns-name' }]);
eq('a format that passes says nothing', GRAMMAR.findings({ traefik: { spec: { host: 'a.example' } } }), []);

// DESIGN section 7: the JSON view renders booleans as 1/0; flagging those would put a
// warning on every boolean in the store.
eq('a boolean on the wire as 1 is not a finding',
    GRAMMAR.findings({ traefik: { spec: { enabled: 1 } } }), []);
eq('a boolean on the wire as 0 is not a finding',
    GRAMMAR.findings({ traefik: { spec: { enabled: 0 } } }), []);
eq('but 2 is', GRAMMAR.findings({ traefik: { spec: { enabled: 2 } } }).length, 1);

eq('keys no grammar describes are left alone',
    GRAMMAR.findings({ traefik: { extra: { anything: [1, 2] } }, mine: { x: 1 } }), []);
eq('a prefix with nothing under it contributes nothing', GRAMMAR.findings({}), []);
eq('a prefix with no schema has none to offer', Shape.of([{ prefix: 'netbird', selector: { all: true } }], []).hasSchema(), false);
eq('... but is still declared', Shape.of([{ prefix: 'netbird', selector: { all: true } }], []).declared().length, 1);

const YAML = [
    'traefik:',
    '  spec:',
    '    host: a.example',
    '    port: 80',
    '  routers:',
    '    - rule: Host(`a`)',
    'netbird:',
    '  groups:',
    '    - lan',
    '',
].join('\n');
const IDX = Markers.lineIndex(YAML);
eq('line index: top level', IDX['traefik'], 1);
eq('line index: nested', IDX['traefik.spec.host'], 3);
eq('line index: sibling after a sequence', IDX['netbird.groups'], 8);
eq('line index: sequence items are not keys', IDX['traefik.routers.rule'], undefined);

const BLOCK = [
    'compose:',
    '  file: |',
    '    services:',
    '      web:',
    '        image: nginx',
    '  name: stack',
    '',
].join('\n');
const BIDX = Markers.lineIndex(BLOCK);
eq('block scalar: the key itself', BIDX['compose.file'], 2);
eq('block scalar: the sibling after it', BIDX['compose.name'], 6);
eq('block scalar: its body is not keys', BIDX['compose.file.services'], undefined);
eq('block scalar: nor promoted to the parent', BIDX['compose.services'], undefined);

const QIDX = Markers.lineIndex('---\n# c\nhost__: note\nhost: a.example\n"quoted: key": 1\n');
eq('comment keys are ordinary keys', QIDX['host__'], 3);
eq('markers and comments are skipped', QIDX['host'], 4);
eq('a quoted key is unquoted', QIDX['quoted: key'], 5);

eq('findings are placed on their lines',
    Markers.placed(GRAMMAR.findings({ traefik: { spec: { port: 70000 } } }),
        Markers.lineIndex('traefik:\n  spec:\n    port: 70000\n')),
    [{ line: 3, message: 'must be at most 65535' }]);
eq('a finding the text does not carry is dropped, not misplaced',
    Markers.placed(GRAMMAR.findings({ traefik: { spec: { port: 70000 } } }),
        Markers.lineIndex('unrelated: 1\n')),
    []);

const SCHEMAS = Object.create(null);
GRAMMAR.schemaIndex().forEach((e) => (SCHEMAS[e.path] = e.schema));
eq('the index lists parents before children', GRAMMAR.schemaIndex().map((e) => e.path).slice(0, 3), ['traefik', 'traefik.spec', 'traefik.spec.host']);
eq('hover: type, range and default',
    Markers.hoverText(SCHEMAS['traefik.spec.port']), 'integer \u00b7 1..65535 \u00b7 default: 80');
eq('hover: type, format and description',
    Markers.hoverText(SCHEMAS['traefik.spec.host']), 'string (dns-name) \u00b7 Public host name');
eq('hover: an enum', Markers.hoverText(SCHEMAS['traefik.spec.scheme']),
    'string \u00b7 one of: http, https');
eq('hover: nothing declared, nothing shown', Markers.hoverText(undefined), null);

console.log('\n--- nesting: most-specific wins, schemas never merge ---');
// `homelab` and `homelab.docker` are both prefixes. The child governs its whole
// subtree; the parent's own `properties.docker` is shadowed, not combined
// (DESIGN section 3). The rule lives in one place (shape::Shape); this shows the
// face over it.
const NESTED = Shape.of([
    {
        prefix: 'homelab',
        selector: { all: true },
        schema: {
            type: 'object',
            properties: {
                notes: { type: 'string' },
                // The parent has an opinion about `docker` -- and must not get one.
                docker: { type: 'string' },
            },
        },
    },
    {
        prefix: 'homelab.docker',
        selector: { all: true },
        schema: { type: 'object', properties: { compose: { type: 'string' } } },
    },
], []);
eq('declared sorts longest prefix first', NESTED.declared().map((n) => n.prefix), ['homelab.docker', 'homelab']);
eq('governing picks the child for the child subtree',
    NESTED.governing('homelab.docker.compose').prefix, 'homelab.docker');
eq('governing picks the parent elsewhere', NESTED.governing('homelab.notes').prefix, 'homelab');
eq('governing picks the child for the boundary itself',
    NESTED.governing('homelab.docker').prefix, 'homelab.docker');
eq('governing returns null off-prefix', NESTED.governing('unrelated.x'), null);
eq('governing returns null for the root', NESTED.governing(''), null);

// The parent declares `docker: string` and the document has a map there. That is a
// finding only if the parent is allowed to reach into the child -- it is not.
const NESTED_DOC = { homelab: { notes: 'ok', docker: { compose: 'services: {}' } } };
eq('the parent does not lint the child subtree', NESTED.findings(NESTED_DOC), []);

// The child does lint its own subtree.
eq('the child lints its own subtree',
    NESTED.findings({ homelab: { docker: { compose: 42 } } }).map((f) => f.path + ': ' + f.msg),
    ['homelab.docker.compose: expected string']);

// And the parent still lints what it does own.
eq('the parent lints its own keys',
    NESTED.findings({ homelab: { notes: 7 } }).map((f) => f.path),
    ['homelab.notes']);

// Hovers resolve to the governing prefix rather than to whichever was collected last.
const NESTED_IDX = Object.create(null);
NESTED.schemaIndex().forEach((e) => (NESTED_IDX[e.path] = e.schema));
eq('hover at the boundary comes from the child',
    NESTED_IDX['homelab.docker'].properties.compose.type, 'string');
eq('hover below the boundary is the child\'s', NESTED_IDX['homelab.docker.compose'].type, 'string');
eq('hover elsewhere is the parent\'s', NESTED_IDX['homelab.notes'].type, 'string');

console.log('\n--- nesting: the ROW builder must shadow too, not just the linter ---');
{
    const nsPanel = Object.assign({}, panel);
    nsPanel.prefixes = [
        {
            prefix: 'homelab.docker',
            selector: { all: true },
            schema: { type: 'object', properties: { compose: { type: 'string' } } },
        },
        {
            prefix: 'homelab',
            selector: { all: true },
            // The parent has an opinion about `docker` and must not get one: the child
            // prefix governs that subtree entirely (DESIGN section 3).
            schema: {
                type: 'object',
                properties: {
                    notes: { type: 'string' },
                    docker: { type: 'string', description: 'the parent should not win here' },
                },
            },
        },
    ];
    const r = { key: '', path: '', children: {}, present: true, kind: 'map' };
    nsPanel.addData.call(nsPanel, r, { homelab: { docker: { compose: 'x' } } });
    // Exactly what buildTree does: one Shape, and the index it hands over is
    // already pruned.
    nsPanel.addShape.call(nsPanel, r, nsPanel.shapeFor('200'));
    const dockerRow = r.children.homelab.children.docker;
    eq('the child governs the boundary row kind', dockerRow.kind, 'map');
    eq('the parent does not describe the child row', dockerRow.grammarDescription, undefined);
    eq('the child declares its own keys', Object.keys(dockerRow.children).sort(), ['compose']);
    eq('the parent still declares its own', r.children.homelab.children.notes.kind, 'string');
}

console.log('\n--- the client model must agree with the server model ---');
// It is the server model: the same parser. A bare date stays the string the store
// keeps (js-yaml's default schema once turned it into a JS Date, and a
// presentation-only toggle then rewrote it).
eq('a bare date stays the string the server stores',
    Codec.parse('date: 2020-01-01\n', 'yaml'), { date: '2020-01-01' });
eq('... and survives a dump/load round trip unchanged',
    Codec.parse(Codec.dump({ date: '2020-01-01' }, 'yaml'), 'yaml'), { date: '2020-01-01' });
eq('a quoted numeric string is still a string', Codec.parse('v: "1"\n', 'yaml'), { v: '1' });
eq('an unquoted integer is still a number', Codec.parse('v: 1\n', 'yaml'), { v: 1 });
eq('booleans still parse', Codec.parse('v: true\n', 'yaml'), { v: true });
eq('YAML 1.1 words stay strings', Codec.parse('a: yes\nb: on\n', 'yaml'), { a: 'yes', b: 'on' });
eq('an empty document is the empty map, not null', Codec.parse('', 'yaml'), {});

console.log('\n--- governing uses containment, not the permission predicate ---');
// `covers` aliases the sibling comment key `p__` -- that is a PERMISSION rule. Using
// it to pick a governing prefix would make `a` govern the whole `a__` prefix; the
// Shape uses plain containment and says `a__`. Two predicates, two jobs, and the
// core keeps them apart (docs: shape.rs vs scopes.rs).
eq('covers aliases the comment key (permission rule)', Access.covers('a', 'a__'), true);
{
    const two = Shape.of([{ prefix: 'a', selector: { all: true } }, { prefix: 'a__', selector: { all: true } }], []);
    eq('a comment-key prefix governs itself, not its subject', two.governing('a__').prefix, 'a__');
    eq('... and `a` alone does not reach `a__`',
        Shape.of([{ prefix: 'a', selector: { all: true } }], []).governing('a__'), null);
}

console.log('\n--- nesting: a schema-less prefix still shadows ---');
{
    // A prefix may declare a selector and no schema.
    // It still governs its subtree -- so a parent's schema must not reach into it.
    const all = Shape.of([
        { prefix: 'homelab.docker', selector: { all: true } },   // no schema
        {
            prefix: 'homelab',
            selector: { all: true },
            schema: {
                type: 'object',
                properties: {
                    notes: { type: 'string' },
                    docker: { type: 'string' },
                },
            },
        },
    ], []);
    const doc = { homelab: { notes: 'ok', docker: { compose: 'x' } } };
    eq('a schema-less child still shadows its parent', all.findings(doc).map((f) => f.path), []);
    eq('the parent still lints what it owns',
        all.findings({ homelab: { notes: 7 } }).map((f) => f.path), ['homelab.notes']);
    eq('and the hover index does not cross the boundary either',
        all.schemaIndex().some((e) => e.path === 'homelab.docker'), false);
}

console.log('\n--- round trip: a view toggle must not invent changes ---');
// The store rewrites a file only when it is asked to write one, so what the editor
// is handed can be a file as somebody *wrote* it -- valid YAML in a layout no
// emitter would choose. Re-dumping that on a presentation toggle invents changes to
// a document nobody edited, which is what `Codec.render` exists to prevent.
const SERVER_YAML = [
    'traefik:',
    '  spec:',
    '    host: "a.example"',
    '    port: 80',
    '  routers:',
    '    - rule: Host(`a`)',
    '',
].join('\n');
const parsed = Codec.parse(SERVER_YAML, 'yaml');
eq('a redump differs from a hand-written file (the bug\'s premise)',
    Codec.dump(parsed, 'yaml') !== SERVER_YAML, true);
eq('same sees through the layout difference',
    Codec.same(parsed, SERVER_YAML), true);
eq('same says no when a value really changed',
    Codec.same({ traefik: { spec: { host: 'b.example' } } }, SERVER_YAML), false);
eq('same says yes when only the key order changed (order is not a value)',
    Codec.same({ b: 1, a: 2 }, 'a: 2\nb: 1\n'), true);
eq('same on unparseable text is not a match',
    Codec.same({}, 'a:\n  - [\n'), false);

// `render` is what both editors' JSON/YAML toggle call: a round trip through JSON
// has to land back on the server's own text, not a fresh dump of it.
eq('render prefers the server text on an unchanged round trip',
    Codec.render(parsed, 'yaml', SERVER_YAML), SERVER_YAML);
eq('render re-dumps once the value actually changed',
    Codec.render({ a: 1 }, 'yaml', SERVER_YAML), Codec.dump({ a: 1 }, 'yaml'));
eq('render for json is a plain dump, original or not',
    Codec.render(parsed, 'json', SERVER_YAML), Codec.dump(parsed, 'json'));
eq('parse reads JSON as JSON and everything else as YAML',
    [Codec.parse('{"a":1}', 'json'), Codec.parse('a: 1\n', 'yaml')],
    [{ a: 1 }, { a: 1 }]);
eq('dump is the plain, unconditional inverse (what Format wants)',
    Codec.dump(parsed, 'yaml') !== SERVER_YAML, true);
eq('originalInLang renders the loaded document in the other syntax',
    Codec.originalInLang(SERVER_YAML, 'json'), Codec.dump(parsed, 'json'));
eq('originalInLang is the identity for yaml -- no reparse, so it never throws',
    Codec.originalInLang(SERVER_YAML, 'yaml'), SERVER_YAML);

console.log('\n--- many documents in one panel ---');
const D = ctx.PVE.meta.DeclareKeyWindow;
// An id is an address: the path it is served at, for every kind of document.
eq('a guest id', P.urlFor.call(P, '201'), '/meta/guests/201');
eq('a prefix id', P.urlFor.call(P, 'prefixes/homelab.docker'), '/meta/prefixes/homelab.docker');
eq('a permission id', P.urlFor.call(P, 'permissions/scoped'), '/meta/permissions/scoped');
eq('kind of a guest', P.docKind.call(P, '201'), 'guest');
eq('kind of a prefix', P.docKind.call(P, 'prefixes/traefik'), 'prefix');
eq('kind of a permission file', P.docKind.call(P, 'permissions/scoped'), 'permission');
eq('the title is the file name', P.docTitle.call(P, 'prefixes/homelab.docker'), 'homelab.docker');

// Per-document digests. One shared field would have sent a prefix's digest with a
// write to another document, which is a 409 at best and the wrong document at worst.
{
    const panelM = Object.assign({}, panel, {
        docState: { 'permissions/y': { digest: 'aaa', data: { a: 1 } }, 'prefixes/x': { digest: 'bbb', data: {} } },
    });
    ['digestOf', 'dataOf', 'docOf'].forEach((m) => (panelM[m] = P[m]));
    panelM.docId = 'permissions/y';
    eq('each document keeps its own digest', panelM.digestOf('prefixes/x'), 'bbb');
    eq('and its own data', panelM.dataOf('permissions/y'), { a: 1 });
    eq('an unknown document has no digest', panelM.digestOf('permissions/nope'), '');
    eq('a row names its document', panelM.docOf({ data: { docId: 'prefixes/x' } }), 'prefixes/x');
    eq('no row means the default one', panelM.docOf(null), 'permissions/y');
}

// What describes a registry document: its meta-schema, rooted at the document.
{
    const META = { type: 'object', properties: { selector: { type: 'object' } } };
    const panelG = Object.assign({}, panel, { registryDoc: true, schemas: { prefix: META, permission: {} } });
    ['shapeFor', 'shapeInputs', 'buildShape', 'docKind'].forEach((m) => (panelG[m] = P[m]));
    eq('a prefix document is described by the meta-schema, rooted at the document',
        panelG.shapeFor('prefixes/x').declared().map((g) => g.prefix), ['']);
    eq('... which is the schema served for its kind',
        panelG.shapeFor('prefixes/x').declared()[0].schema, META);
}

// A root-rooted schema governs the whole document. There is no special case for it:
// the empty prefix is a prefix of everything, and the least specific of all.
{
    const META = {
        type: 'object',
        properties: {
            selector: { type: 'object', properties: { tag: { type: 'string' } } },
            description: { type: 'string' },
        },
    };
    const rooted = Shape.rooted(META);
    eq('the meta-schema governs everything, the root included', [rooted.governing('').prefix, rooted.governing('rules.0').prefix], ['', '']);
    eq(
        'the meta-schema lints the document it is rooted at',
        rooted.findings({ description: 5, selector: { tag: 7 } }).map((f) => f.path + ': ' + f.msg),
        ['description: expected string', 'selector.tag: expected string'],
    );
    eq(
        'and indexes it for hovers',
        rooted.schemaIndex().map((e) => e.path).sort(),
        ['', 'description', 'selector', 'selector.tag'],
    );
    eq('a missing meta-schema (an older API) describes nothing', Shape.rooted(undefined).declared(), []);
    // The same rows the tree would show, including a declared-but-unset one.
    const panelR = Object.assign({}, panel, {
        registryDoc: true,
        docState: { 'prefixes/x': { digest: 'd', data: { selector: { tag: 'traefik' } } } },
        schemas: { prefix: META },
    });
    ['shapeFor', 'shapeInputs', 'buildShape', 'docKind', 'documentEntries', 'addData', 'addShape', 'entry', 'schemaKind', 'dataOf', 'plannedData'].forEach(
        (m) => (panelR[m] = P[m]),
    );
    panelR.pending = EditSet.empty();
    panelR.docId = 'prefixes/x';
    const entries = panelR.documentEntries();
    eq('a prefix document shows its declared keys', Object.keys(entries.children).sort(), ['description', 'selector']);
    eq('what it holds is present', entries.children.selector.children.tag.present, true);
    eq('what it does not hold is a declared-but-unset row', entries.children.description.present, false);
    eq('the root row itself is untouched', entries.kind, 'map');
}

console.log('\n--- a marker has to survive collapsing the branch it is in ---');
{
    // The whole point: collapse `homelab` and the amber `port` goes with it, along
    // with any sign that something is wrong. Every ancestor carries the count.
    const rolled = U.rollUp({
        'homelab.docker.port': 'must be at most 65535',
        'homelab.owner': 'expected string',
    });
    eq('every ancestor answers for what is under it', Object.keys(rolled).sort(), [
        'homelab',
        'homelab.docker',
    ]);
    eq('the top of the branch counts both', rolled.homelab.count, 2);
    eq('a nearer branch counts only its own', rolled['homelab.docker'].count, 1);
    eq('and the tooltip names them', rolled['homelab.docker'].messages, [
        'homelab.docker.port: must be at most 65535',
    ]);
    // A top-level finding has no ancestor to bubble to -- and must not invent one.
    eq('a top-level finding bubbles nowhere', U.rollUp({ owner: 'expected string' }), {});
    eq('nothing wrong, nothing to say', U.rollUp({}), {});

    // The tooltip shows the first few, not a wall of text.
    const many = {};
    for (let i = 0; i < 7; i++) {
        many['a.b.k' + i] = 'expected string';
    }
    const deep = U.rollUp(many);
    eq('all of them are counted', deep.a.count, 7);
    eq('but only the first few are quoted', deep.a.messages.length, 3);
}

console.log('\n--- staged edits: the change that had no legal single step ---');
{
    // The case that forced this: a prefix definition's selector is exactly one of
    // `all` or `tag`, so `{all: true}` -> `{tag: web}` has NO valid intermediate.
    // Dropping `all` first is refused by the server; adding `tag` first is refused;
    // the row editor could only ever do one at a time.
    const stored = { description: 'Home', selector: { all: true } };
    const pending = new EditSet([
        { path: 'selector.all', op: 'delete' },
        { path: 'selector.tag', op: 'set', value: 'web' },
    ]);
    eq('both edits land in one planned document', pending.apply(stored), {
        description: 'Home',
        selector: { tag: 'web' },
    });
    // ... and go out as ONE write, at the narrowest view covering both.
    eq('written as one view', pending.writeView(), 'selector');
    eq('the stored document is untouched until then', stored, {
        description: 'Home',
        selector: { all: true },
    });

    const set = (edits) => new EditSet(edits);
    // A single row edit is still exactly the one-key write it always was.
    eq('one set writes that key', set([{ path: 'a.b.c', op: 'set', value: 1 }]).writeView(), 'a.b.c');
    // A delete cannot be expressed by replacing the thing being deleted, so the
    // write moves one level up and replaces the parent without the key.
    eq('one delete writes its parent', set([{ path: 'a.b.c', op: 'delete' }]).writeView(), 'a.b');
    eq('a top-level delete writes the document', set([{ path: 'a', op: 'delete' }]).writeView(), '');
    eq('unrelated subtrees write the document', set([
        { path: 'traefik.spec.host', op: 'set', value: 'x' },
        { path: 'netbird.groups', op: 'set', value: [] },
    ]).writeView(), '');
    eq('nothing staged, nothing to write', EditSet.empty().writeView(), null);
    eq('... and an empty set says so', [EditSet.empty().isEmpty(), EditSet.empty().length], [true, 0]);

    // Deletes and sets applied in the order they were made.
    eq('order is what was done', set([
        { path: 'a', op: 'delete' },
        { path: 'a', op: 'set', value: 2 },
    ]).apply({ a: 1 }), { a: 2 });
    eq('a set then a delete leaves nothing', set([
        { path: 'x.y', op: 'set', value: 1 },
        { path: 'x.y', op: 'delete' },
    ]).apply({}), { x: {} });
    // Intermediate maps are created for a new nested key.
    eq('a new nested key builds its parents', set([
        { path: 'schema.properties.port.type', op: 'set', value: 'integer' },
    ]).apply({}), { schema: { properties: { port: { type: 'integer' } } } });

    // Keys are document data and no key is reserved: staging one named `__proto__`
    // must set a key, not the prototype. The document crosses as JSON text, so there
    // is no object for a prototype setter to touch on the way -- but the answer has
    // to come back as a key too. (`entry()` guards the same way on its side.)
    const planned = set([{ path: '__proto__', op: 'set', value: 'oops' }]).apply({});
    eq('a proto-named key is a key', Object.prototype.hasOwnProperty.call(planned, '__proto__'), true);
    eq('and the prototype is untouched', {}.oops, undefined);
    eq('and Object still is Object', Object.getPrototypeOf({}), Object.prototype);
}

console.log('\n--- the registry lists ---');
{
    // `statics:` in the shim is a plain object; Ext hoists it onto the class.
    const G = ctx.PVE.meta.RegistryGrid.statics;
    const rows = G.rowsFrom('prefixes', [
        {
            prefix: 'traefik',
            description: 'Traefik dynamic configuration',
            selector: { tag: 'traefik' },
            schema: { type: 'object' },
            origin: 'packaged',
            overrides: false,
        },
        { prefix: 'netbird', selector: { all: true }, origin: 'cluster', overrides: false },
        { prefix: 'homelab', selector: { all: true }, schema: {}, origin: 'cluster', overrides: true },
    ]);
    eq('a row is addressed by the document id it opens', rows.map((r) => r.id), [
        'prefixes/traefik',
        'prefixes/netbird',
        'prefixes/homelab',
    ]);
    eq('the selector is the "applies to" column', rows.map((r) => r.selector), [
        'tag: traefik',
        'all guests',
        'all guests',
    ]);
    // A column a tree could not show: a schema-less definition is a real thing,
    // indistinguishable from one with a schema without it.
    eq('carrying a schema is a column', rows.map((r) => r.schema), ['yes', '', 'yes']);

    // Three origin states, not two. The middle one is where Remove does not remove.
    eq('a package\'s file', G.originText(rows[0]), 'packaged');
    eq('an administrator\'s own', G.originText(rows[1]), 'cluster');
    eq('one written over a package\'s', G.originText(rows[2]), 'cluster (overrides packaged)');

    const permRows = G.rowsFrom('permissions', [
        {
            name: 'scoped',
            authid: 'svc@pve!t1',
            rules: [
                { prefix: 'traefik', mode: 'rw', selector: { tag: 'traefik' } },
                { prefix: 'netbird', mode: 'ro', selector: { all: true } },
            ],
            origin: 'cluster',
        },
    ]);
    eq('a permission row is addressed the same way', permRows[0].id, 'permissions/scoped');
    eq(
        'and says what it actually permits',
        permRows[0].summary,
        'traefik (rw, tag: traefik), netbird (ro, all guests)',
    );
    // An older API returns neither field; the list must still render.
    eq('a row with no origin is treated as the cluster\'s', G.originText(G.rowsFrom('permissions', [{ name: 'x' }])[0]), 'cluster');
}

console.log('\n--- a key name is refused in the field, not after a round trip ---');
{
    // The rule is the core's (`path::is_valid_segment`); this only puts it into
    // words. `invalid path: homelab.bad key (400)` is a correct answer that reads
    // like a bug in the editor.
    eq('a plain key is fine', U.keyPathError('homelab'), null);
    eq('a dotted path is fine', U.keyPathError('homelab.docker.port'), null);
    eq('the charset is the server\'s', U.keyPathError('a-b_c@d!e9'), null);
    eq('an empty key is refused', typeof U.keyPathError(''), 'string');
    eq('a space is refused', typeof U.keyPathError('bad key'), 'string');
    eq('... and the message names it', U.keyPathError('bad key').indexOf('space') !== -1, true);
    eq('... and the segment', U.keyPathError('ok.bad key').indexOf('bad key') !== -1, true);
    eq('a slash is refused', typeof U.keyPathError('a/b'), 'string');
    eq('an empty segment is refused', typeof U.keyPathError('a..b'), 'string');

    // Comment keys are ordinary keys under this charset, and the editor must not
    // refuse the one spelling the document model is built on (DESIGN section 2).
    eq('a comment key is fine', U.keyPathError('documented__'), null);
    eq('the bare document comment key is fine', U.keyPathError('__'), null);

    // Non-ASCII is refused, the same way the server refuses it. The `KEYS` corpus
    // above is about what the YAML *codec* must round-trip -- a stored document can
    // have arrived by hand or from an older writer -- which is a wider set than what
    // a path may name. This field creates a key, so it is bound by the narrower rule.
    eq('non-ascii is refused', typeof U.keyPathError('\u00fcn\u00efc\u00f8de'), 'string');

    // A registry file name is a dotted prefix (`registry::is_valid_file_name`): the
    // rule the loader, the API id and the New dialog all apply -- the dialog through
    // the same function now, rather than through no check at all.
    eq('a file name is a prefix', U.fileNameError('homelab.docker'), null);
    eq('a plain one too', U.fileNameError('traefik'), null);
    eq('a space is not a file name', typeof U.fileNameError('my file'), 'string');
    eq('nor a slash', typeof U.fileNameError('a/b'), 'string');
    eq('nor a leading dot', typeof U.fileNameError('.hidden'), 'string');
    eq('nor nothing', typeof U.fileNameError(''), 'string');
}

console.log('\n--- an edit answers for what it broke, not for what was already broken ---');
{
    // `Shape.introduced` scopes the banner to what this edit did; the amber row
    // markers still show everything wrong with the document.
    const stored = {
        homelab: { owner: 'arki', port: 'not-a-number' },
        netbird: { groups: ['lan'] },
    };
    const bad = { path: 'homelab.port', msg: 'expected integer' };
    const before = [bad];

    // An unrelated edit: the same violation is still there, and it is not ours.
    eq(
        'a pre-existing violation on an untouched path does not warn',
        Shape.introduced(before, [bad], EditSet.changedPaths(stored, {
            ...stored,
            homelab: { ...stored.homelab, owner: 'someone' },
        })),
        [],
    );

    // The same path, a different wrong value: ours this time, even though the message
    // is word for word what it was.
    eq(
        'a new bad value on an already-bad path does warn',
        Shape.introduced(before, [bad], EditSet.changedPaths(stored, {
            ...stored,
            homelab: { ...stored.homelab, port: 'still-not-a-number' },
        })).length,
        1,
    );

    // Replacing a parent answers for what is beneath it.
    eq(
        'replacing a subtree answers for a finding inside it',
        Shape.introduced(before, [bad], ['homelab']).length,
        1,
    );

    // A violation that was not there before always warns, wherever it is.
    const fresh = { path: 'netbird.groups', msg: 'expected array' };
    eq(
        'a violation this edit created always warns',
        Shape.introduced(before, [bad, fresh], ['netbird.groups']).map((f) => f.path),
        ['netbird.groups'],
    );

    // A pure key reordering changes no value at any path, so it introduces nothing.
    eq(
        'reordering changes no path',
        EditSet.changedPaths(stored, { netbird: stored.netbird, homelab: stored.homelab }),
        [],
    );
    eq(
        'so reordering a document that was already wrong warns about nothing',
        Shape.introduced(before, [bad], []),
        [],
    );

    // `changedPaths` reports the deepest path that differs, and compares lists whole.
    eq(
        'a changed leaf is reported at its own path',
        EditSet.changedPaths(stored, { ...stored, homelab: { ...stored.homelab, owner: 'x' } }),
        ['homelab.owner'],
    );
    eq(
        'a changed list member is reported at the list',
        EditSet.changedPaths(stored, { ...stored, netbird: { groups: ['lan', 'wan'] } }),
        ['netbird.groups'],
    );
    eq(
        'a removed key is a change at that key',
        EditSet.changedPaths(stored, { homelab: stored.homelab }),
        ['netbird'],
    );
}

console.log('\n--- text is just another way to edit rows ---');
{
    // `EditSet.between` turns whatever was typed back into edits *on rows*, so
    // both are the same model and the rules go away.
    const stored = {
        homelab: { owner: 'arki', notes: 'the box', docker: { port: 80, restart: 'always' } },
        netbird: { groups: ['lan'] },
    };
    const d = (edited) => EditSet.between(stored, edited).edits;

    eq('an unchanged document stages nothing', d(JSON.parse(JSON.stringify(stored))), []);

    // A one-key change stays a one-key edit, so the tree marks that row and no other.
    eq(
        'a changed leaf is one edit on its own path',
        d({ ...stored, homelab: { ...stored.homelab, owner: 'someone' } }),
        [{ path: 'homelab.owner', op: 'set', value: 'someone' }],
    );
    // A key that is gone comes back as a delete, which is what draws it struck through.
    const withoutNotes = { ...stored, homelab: { owner: 'arki', docker: stored.homelab.docker } };
    eq('a removed key is a delete', d(withoutNotes), [{ path: 'homelab.notes', op: 'delete' }]);
    eq(
        'a new key is a set at its full path',
        d({ ...stored, homelab: { ...stored.homelab, tags: 'x' } }),
        [{ path: 'homelab.tags', op: 'set', value: 'x' }],
    );
    // Lists are compared whole: their members are not addressable (DESIGN §2).
    eq(
        'a changed list is one edit on the list',
        d({ ...stored, netbird: { groups: ['lan', 'wan'] } }),
        [{ path: 'netbird.groups', op: 'set', value: ['lan', 'wan'] }],
    );

    // Key order is not a value: a pure reordering is the same document and stages
    // nothing (docs/decisions/007-key-order-is-kept-not-meaning.md).
    const reordered = { netbird: stored.netbird, homelab: stored.homelab };
    eq('a pure reordering stages nothing', EditSet.between(stored, reordered).length, 0);

    // Whatever comes back, replaying it on the stored document must equal what was
    // typed -- that is the property the fallback exists to guarantee.
    [
        { ...stored, homelab: { ...stored.homelab, docker: { port: 8080, restart: 'no' } } },
        { homelab: stored.homelab },
        {},
    ].forEach(function (edited, i) {
        eq('case ' + i + ' round trips', EditSet.between(stored, edited).apply(stored), edited);
    });

    // A root-level edit replaces the whole document, so a staged edit under a key it
    // does not have must not be re-applied on top. The edit set is derived from the
    // result, so what it holds is the *minimal* description of that -- the keys that
    // went, and the one that arrived -- rather than one opaque root edit. The planned
    // document is the assertion; the shape is how the tree gets to mark the rows that
    // actually differ.
    const stub = stagingStub(stored, [{ path: 'homelab.owner', op: 'set', value: 'x' }]);
    stub.stage('', 'set', { a: 1 });
    eq('the planned document is what was staged', stub.pending.apply(stored), { a: 1 });
    eq(
        'and it is described minimally',
        stub.pending.edits.map((e) => e.path + ':' + e.op).sort(),
        ['a:set', 'homelab:delete', 'netbird:delete'],
    );
    eq('an edit under a replaced key is not re-applied on top', stub.pending.apply(stored).homelab, undefined);

    // Dirty means the document differs, not that an edit exists. Staging the value a
    // row already has -- which is what the subtree editor's OK does when you typed
    // nothing -- must leave nothing staged, or Revert appears with no row marked.
    const noop = stagingStub(stored);
    noop.stage('', 'set', JSON.parse(JSON.stringify(stored)));
    eq('staging the document it already is stages nothing', noop.pending.edits, []);
    noop.stage('homelab.owner', 'set', stored.homelab.owner);
    eq('... and so does staging a value a row already has', noop.pending.edits, []);

    // And the round trip: A to B and back to A cancels, rather than leaving two edits
    // that together describe no change.
    noop.stage('homelab.owner', 'set', 'changed');
    eq('a real change still stages', noop.pending.length, 1);
    noop.stage('homelab.owner', 'set', stored.homelab.owner);
    eq('changing it back cancels the edit', noop.pending.edits, []);

    // A delete is staged without a value, and is a real change.
    noop.stage('netbird', 'delete');
    eq('a delete is staged without a value', noop.pending.edits, [{ path: 'netbird', op: 'delete' }]);
    eq('... and removes the key from the planned document', noop.pending.apply(stored).netbird, undefined);

}

console.log('\n--- a single delete has to stay a DELETE ---');
{
    // `writeView` steps up a level for a delete -- you cannot remove a key by replacing
    // it -- but for a top-level key that step lands on the document root, and a root
    // write needs full write access. A scoped writer removing its own prefix would get
    // a 403 for something the server would have taken as `DELETE ?view=traefik`.
    eq('a top-level delete would write the document', new EditSet([{ path: 'traefik', op: 'delete' }]).writeView(), '');
    // ... so Apply sends the narrow DELETE instead, which is what this shape is for.
    eq('a nested delete writes its parent', new EditSet([{ path: 'a.b', op: 'delete' }]).writeView(), 'a');
    // Two edits are a replace again: only a lone delete has a narrower spelling.
    eq(
        'a delete beside a set is not one',
        new EditSet([{ path: 'a', op: 'delete' }, { path: 'b', op: 'set', value: 1 }]).writeView(),
        '',
    );
}

console.log('\n--- a staged value is linted like a stored one ---');
{
    // Findings are computed against the *planned* document, so a value that breaks
    // the schema is marked the moment it is staged -- not after it is written.
    const SCHEMA = { type: 'object', properties: { port: { type: 'integer', maximum: 65535 } } };
    const panelS = Object.assign({}, panel, {
        registryDoc: false,
        docId: '201',
        docState: { 201: { digest: 'd', data: { docker: { port: 80 } } } },
        prefixes: [{ prefix: 'docker', selector: { all: true }, schema: SCHEMA }],
        tags: [],
        pending: EditSet.empty(),
    });
    ['shapeFor', 'shapeInputs', 'buildShape', 'findingsFor', 'docKind', 'dataOf', 'plannedData'].forEach((m) => (panelS[m] = P[m]));

    eq('a stored value that fits is not marked', panelS.findingsFor()['docker.port'], undefined);
    panelS.pending = new EditSet([{ path: 'docker.port', op: 'set', value: 70000 }]);
    eq('a staged value that does not fit is', panelS.findingsFor()['docker.port'], 'must be at most 65535');
    // ... and it is still allowed to be staged and applied: the marker is advisory,
    // the server's lint is the authority (DESIGN §7).
    eq('the planned document keeps it', panelS.plannedData().docker.port, 70000);

}

console.log('\n--- discarding a row puts the stored value back there, and nothing else ---');
{
    // The edit set is the difference, so "drop this row's edit" and "put back what is
    // stored here" are one operation -- and it leaves every other row's edit alone.
    const stored = { docker: { port: 80, host: 'h' }, a: 1, ab: 2 };
    const paths = (s) => s.pending.edits.map((e) => e.path + ':' + e.op).sort();
    const stub = stagingStub(stored, [
        { path: 'docker.port', op: 'set', value: 70000 },
        { path: 'docker.host', op: 'set', value: 'x' },
        { path: 'added', op: 'set', value: 1 },
        { path: 'a', op: 'set', value: 9 },
        { path: 'ab', op: 'delete' },
    ]);
    stub.discardRow({ data: { path: 'docker.port' } });
    eq('discarding a row drops its edit and keeps the rest', paths(stub), ['a:set', 'ab:delete', 'added:set', 'docker.host:set']);
    eq('... and the row shows what is stored', stub.plannedData().docker.port, 80);
    stub.discardRow({ data: { path: 'added' } });
    eq('a key that was added goes away', [paths(stub).includes('added:set'), stub.plannedData().added], [false, undefined]);
    stub.discardRow({ data: { path: 'ab' } });
    eq('a ghost gets its value back', stub.plannedData().ab, 2);
    eq('`ab` is not under `a`: `a` is still staged', paths(stub), ['a:set', 'docker.host:set']);
    stub.discardRow({ data: { path: 'netbird' } });
    eq('an untouched row changes nothing', paths(stub), ['a:set', 'docker.host:set']);
    stub.discardRow({ data: { path: 'docker' } });
    eq('discarding a subtree restores everything under it', paths(stub), ['a:set']);
    eq('... which is what dirty means', stub.isDirty(), true);
    stub.discardRow({ data: { path: 'a' } });
    eq('and the last one leaves nothing staged', [stub.isDirty(), stub.plannedData()], [false, stored]);
}

console.log('\n--- acting on one member rewrites its list ---');
{
    // There is no path to `groups[1]`, so every action on a member is a write of the
    // whole list. Staging is what makes that unremarkable: it is one more staged
    // edit, applied with everything else.
    const stub = {
        docId: '201',
        pending: EditSet.empty(),
        docState: { 201: { digest: 'd', data: { netbird: { groups: ['lan', 'wan', 'dmz'] } } } },
    };
    ['listAt', 'stageListMember', 'plannedData', 'dataOf'].forEach((m) => (stub[m] = P[m]));
    // A recording stub, not the real `stage`: it keeps every edit so the test
    // can read the second one, where the real thing would have replaced the first.
    stub.stage = function (path, op, value) {
        this.pending.edits.push({ path: path, op: op, value: value });
    };

    eq('the list as it stands', stub.listAt('netbird.groups'), ['lan', 'wan', 'dmz']);
    stub.stageListMember('netbird.groups', 1, 'wlan');
    eq('editing a member writes the list', stub.pending.edits[0], {
        path: 'netbird.groups',
        op: 'set',
        value: ['lan', 'wlan', 'dmz'],
    });
    // ... and it reads back through the staged edit, so a second action composes.
    eq('and the next action sees it', stub.listAt('netbird.groups'), ['lan', 'wlan', 'dmz']);
    stub.stageListMember('netbird.groups', 0, undefined);
    eq('removing a member drops it', stub.pending.edits[1].value, ['wlan', 'dmz']);
    // An index that is not there changes nothing, rather than growing the list with
    // a hole in it.
    stub.pending = EditSet.empty();
    stub.stageListMember('netbird.groups', 9, 'nope');
    eq('an index that is not there is not an edit', stub.pending.edits, []);
    stub.stageListMember('netbird.groups', -1, 'nope');
    eq('nor is a negative one', stub.pending.edits, []);
}

console.log('\n--- a delete leaves no container the stored document does not have ---');
{
    // Set a ghost `docker.compose` on a document with no `docker`, then discard it:
    // the map the first edit created on the way must go with the key, or `docker: {}`
    // stays staged and Apply writes a map the guest never asked for. A map the file
    // does hold is a stored state and stays, empty or not.
    const s = stagingStub({ keep: { a: 1 }, empty: {} });
    s.stage('docker.compose', 'set', 'x');
    eq('the edit created the container', s.plannedData().docker, { compose: 'x' });
    s.discardRow({ data: { path: 'docker.compose' } });
    eq('discarding the ghost leaves nothing staged', [s.pending.edits, s.isDirty()], [[], false]);
    s.stage('deep.er.key', 'set', 1);
    s.stage('deep.er.key', 'delete');
    eq('removing the only key removes the chain it created', s.pending.edits, []);
    s.stage('keep.a', 'delete');
    eq('a stored map stays when its last key goes', s.plannedData().keep, {});
    s.stage('empty.new', 'set', 1);
    s.stage('empty.new', 'delete');
    eq('... and a stored empty map is not touched either',
        [s.plannedData().empty, s.pending.edits.filter((e) => e.path.indexOf('empty') === 0)], [{}, []]);
    eq('parentPath', [U.parentPath('a.b.c'), U.parentPath('a'), U.parentPath('')], ['a.b', '', '']);
}

console.log('\n--- discarding one member puts that member back, not the list ---');
{
    // The edit is staged on the *list*, so dropping it would throw away every other
    // member's change. Discard puts that one member back instead, and the list's
    // edit is gone once every member matches again -- a list that matches is not a
    // difference, so that needs no step of its own.
    const stored = { netbird: { groups: ['lan', 'wan'] }, k: 1 };
    const s = stagingStub(stored);
    const groups = () => s.plannedData().netbird.groups;
    s.stageListMember('netbird.groups', 0, 'LAN');
    s.stageListMember('netbird.groups', 1, 'WAN');
    s.stage('netbird.groups', 'set', groups().concat(['dmz']));
    eq('three members changed, one edit', [groups(), s.pending.length], [['LAN', 'WAN', 'dmz'], 1]);
    s.discardRow({ data: { path: 'netbird.groups', arrayIndex: 0 } });
    eq('discarding one member restores it and not the list', groups(), ['lan', 'WAN', 'dmz']);
    s.discardListMember('netbird.groups', 2);
    eq('discarding an appended member removes it', groups(), ['lan', 'WAN']);
    s.discardListMember('netbird.groups', 1);
    eq('once every member matches, nothing is staged', s.pending.edits, []);

    // Revert: the document you are looking at is the stored one.
    s.stage('netbird.groups', 'set', ['x']);
    s.stage('k', 'delete');
    s.stage('new', 'set', true);
    eq('three edits staged', s.pending.length, 3);
    s.revertPending();
    eq('Revert clears everything', [s.pending.edits, s.plannedData()], [[], stored]);
}

console.log('\n--- a list is a container, like a map ---');
{
    // The tree existed to make a document something you can look at and act on one
    // piece of. A list was the one shape that stayed a blob of JSON in a cell, for
    // no reason other than that it came second.
    const root = { key: '', path: '', children: {}, present: true, kind: 'map' };
    panel.addData.call(panel, root, {
        netbird: { groups: ['lan', 'wan'] },
        rules: [
            { prefix: 'traefik', mode: 'rw', selector: { tag: 'traefik' } },
            { prefix: 'netbird', mode: 'ro', selector: { all: true } },
        ],
        empty: [],
    });

    const groups = root.children.netbird.children.groups;
    eq('a list of scalars has a row per member', Object.keys(groups.children).sort(), ['0', '1']);
    eq('each member keeps its own value', groups.children['0'].value, 'lan');
    eq('and knows which member it is', groups.children['1'].arrayIndex, 1);
    // Not addressable: a view addresses through maps only, so nothing may try to
    // write `groups.1` -- everything that acts on a member rewrites the list.
    eq('a member is not addressable', groups.children['0'].addressable, false);
    eq('the list itself still is', groups.addressable, undefined);

    const rules = root.children.rules;
    eq('a list of maps too', Object.keys(rules.children).sort(), ['0', '1']);
    // A member with structure shows one readable line, and carries its real value.
    eq('a rule reads as a rule', rules.children['0'].value, 'traefik (rw, tag: traefik)');
    eq('and the real thing rides along', rules.children['0'].rawItem.selector, { tag: 'traefik' });
    eq('an empty list has no members', Object.keys(root.children.empty.children), []);

    // The summary is presentation only, and falls back to JSON for a shape it does
    // not recognise -- it describes nothing and constrains nothing.
    eq('an unknown shape is still legible', U.itemSummary({ a: 1 }), '{"a":1}');
    eq('a scalar member is itself', U.itemSummary('lan'), 'lan');
}

console.log('\n--- adding one rule to a permission file ---');
{
    const R = ctx.PVE.meta.AddRuleWindow.statics;
    eq('a rule with an all selector', R.rulesWith([], { prefix: 'traefik', mode: 'rw', selector: 'all' }), [
        { prefix: 'traefik', mode: 'rw', selector: { all: true } },
    ]);
    eq('a rule with a tag selector', R.rulesWith([], { prefix: 'homelab', mode: 'ro', selector: 'tag', tag: 'web' }), [
        { prefix: 'homelab', mode: 'ro', selector: { tag: 'web' } },
    ]);
    // Appending, not replacing: `rules` is written whole because a view addresses
    // through maps only, so the existing entries have to come along.
    eq(
        'the ones already there come with it',
        R.rulesWith([{ prefix: 'netbird', mode: 'ro', selector: { all: true } }], {
            prefix: 'traefik', mode: 'rw', selector: 'all',
        }).map((r) => r.prefix),
        ['netbird', 'traefik'],
    );
    eq('a missing mode is the safe one', R.rulesWith([], { prefix: 'x', selector: 'all' })[0].mode, 'ro');
    eq('nothing there yet is still an array', Array.isArray(R.rulesWith(undefined, { prefix: 'x', selector: 'all' })), true);
}

console.log('\n--- creating a registry file: the least that parses ---');
{
    const plan = (kind, v) => ctx.PVE.meta.NewRegistryWindow.statics.planFrom(kind, v);
    eq('a prefix with an "all" selector', plan('prefixes', { name: 'x', selector: 'all' }).content, { selector: { all: true } });
    eq('a prefix with a tag selector', plan('prefixes', { name: 'x', selector: 'tag', tag: 'web' }).content, { selector: { tag: 'web' } });
    eq(
        'a description when there is one',
        plan('prefixes', { name: 'x', selector: 'all', description: 'Home' }).content,
        { description: 'Home', selector: { all: true } },
    );
    // A permission file is created permitting nothing: it names a principal, and an
    // administrator says what it may touch afterwards.
    const existing = plan('permissions', { name: 'ops', principal: 'existing', authid: 'a@pve!t1' });
    eq('a permission file starts empty', existing.content, { authid: 'a@pve!t1', rules: [] });
    eq('an existing principal makes nothing', existing.user, undefined);
    eq('the file is named separately from the principal', existing.file, 'ops');

    // The other half of the same dialog: the principal does not exist yet. The file
    // must name the TOKEN, not the user -- naming the user produces a file that
    // parses, loads, and grants the token nothing.
    const fresh = plan('permissions', {
        name: 'traefik', principal: 'new', user: 'traefik@pve', tokenid: 'meta', role: 'none',
    });
    eq('the file names the token', fresh.content.authid, 'traefik@pve!meta');
    eq('and the user is created too', fresh.user, 'traefik@pve');
    eq('the none sentinel grants nothing', fresh.acl, undefined);
    const withRole = plan('permissions', {
        name: 'a', principal: 'new', user: 'a@pve', tokenid: 't', role: 'PVEAuditor', description: 'x',
    });
    // /vms, not per-guest (a guest created tomorrow would miss it) and not `/`
    // (PVEAuditor there also grants Sys.Audit, the datacenter document's read).
    eq('a role goes on /vms, propagating', withRole.acl, { path: '/vms', role: 'PVEAuditor', propagate: 1 });
    eq('a description reaches the file', withRole.content.description, 'x');

    // The generated token id is a name, not a secret: PVE generates the secret.
    const T = ctx.PVE.meta.ServiceToken;
    eq('a generated id is a legal token id', /^[A-Za-z0-9_-]+$/.test(T.randomTokenId()), true);
    eq('and two of them differ', T.randomTokenId() === T.randomTokenId(), false);
}

console.log('\n--- reloading must not fold the tree up ---');
{
    // The key that survives a reload. A document row and a group row both live at the
    // empty path, and the two group rows have no document at all, so keying on
    // (docId, path) collided: collapsing `Prefixes` came back expanded on the next
    // reload because `Grants` had won the shared key.
    const key = (n) => (n.data.docId || '') + '\u0000' + n.data.path + '\u0000' + (n.data.key || '');
    const prefixes = { data: { docId: null, path: '', key: 'Prefixes' } };
    const permissions = { data: { docId: null, path: '', key: 'Permissions' } };
    const dcRoot = { data: { docId: 'permissions/scoped', path: '', key: 'permissions/scoped' } };
    const nsRoot = { data: { docId: 'prefixes/homelab', path: '', key: 'prefixes/homelab' } };
    const same = { data: { docId: 'prefixes/homelab', path: 'selector', key: 'selector' } };
    const other = { data: { docId: 'permissions/scoped', path: 'selector', key: 'selector' } };
    const keys = [prefixes, permissions, dcRoot, nsRoot, same, other].map(key);
    eq('every row has a key of its own', new Set(keys).size, keys.length);
}

console.log('\n--- the tree marks a row its schema refuses ---');
{
    // Same rule, same function -- `shapeFor` is shared by both callers so they
    // cannot answer differently.
    const SCHEMA = {
        type: 'object',
        properties: { port: { type: 'integer', minimum: 1, maximum: 65535 } },
    };
    const panelF = Object.assign({}, panel, {
        registryDoc: false,
        docState: { 201: { digest: 'd', data: { docker: { port: 70000, host: 'ok' } } } },
        prefixes: [{ prefix: 'docker', selector: { all: true }, schema: SCHEMA }],
        tags: [],
    });
    ['shapeFor', 'shapeInputs', 'buildShape', 'findingsFor', 'docKind', 'dataOf', 'plannedData'].forEach(
        (m) => (panelF[m] = P[m]),
    );
    panelF.pending = EditSet.empty();
    panelF.docId = '201';
    const found = panelF.findingsFor();
    eq('the out-of-range row is marked', found['docker.port'], 'must be at most 65535');
    eq('a row that fits is not', found['docker.host'], undefined);

    // A registry document is linted by its meta-schema through the same call.
    const panelR = Object.assign({}, panel, {
        registryDoc: true,
        docState: { 'prefixes/x': { digest: 'd', data: { description: 5 } } },
        schemas: { prefix: { type: 'object', properties: { description: { type: 'string' } } } },
    });
    ['shapeFor', 'shapeInputs', 'buildShape', 'findingsFor', 'docKind', 'dataOf', 'plannedData'].forEach(
        (m) => (panelR[m] = P[m]),
    );
    panelR.pending = EditSet.empty();
    panelR.docId = 'prefixes/x';
    eq(
        'a prefix file is marked against the meta-schema',
        panelR.findingsFor().description,
        'expected string',
    );
    // And the banner Apply shows asks the same Shape, through `applyFindingsFor`.
    panelF.applyFindingsFor = P.applyFindingsFor;
    eq('the banner names what an edit introduced',
        panelF.applyFindingsFor({ docker: { port: 70000, host: 5 } }),
        []);
    eq('... but not what was already wrong and untouched',
        panelF.applyFindingsFor({ docker: { port: 70000, host: 'changed' } }),
        []);
    eq('... and a fresh violation, wherever it is',
        panelF.applyFindingsFor({ docker: { port: 'eighty', host: 'ok' } }),
        ['docker.port: expected integer']);
}

console.log('\n--- declaring one key of a prefix schema ---');
eq('the type is always written', D.schemaFrom({ type: 'string' }), { type: 'string' });
eq(
    'every field the editor consumes',
    D.schemaFrom({
        type: 'integer',
        description: 'How many',
        default: '3',
        minimum: '1',
        maximum: '9',
    }),
    { type: 'integer', description: 'How many', default: 3, minimum: 1, maximum: 9 },
);
// There is no Optional field, and a stray one is not written: every key of a guest
// document is optional, so `optional` would be a claim nothing reads or enforces.
eq('optional is never declared', D.schemaFrom({ type: 'string', optional: true }), { type: 'string' });

// Visibility and enforcement are three-state, because the rule is inherit unless
// this node says otherwise. "Inherit" writes nothing; `false` is a real statement
// -- stop inheriting and be visible, or be advisory -- so it is not the same as
// leaving it out, and neither is a default.
eq('inherit writes nothing', D.schemaFrom({ type: 'string', hidden: 'inherit', enforce: 'inherit' }),
    { type: 'string' });

// `valuesFrom` is `schemaFrom` backwards, and the pair has to agree about every
// field: one the reverse forgot is a field that silently resets the moment somebody
// edits a declaration rather than writing a new one.
[
    { type: 'string' },
    { type: 'string', description: 'a host', format: 'dns-name', multiline: 1 },
    { type: 'integer', minimum: 1, maximum: 65535, default: 80 },
    { type: 'boolean', default: true },
    { type: 'string', enum: ['http', 'https'], default: 'http' },
    { type: 'object', hidden: true, enforce: false },
    { type: 'string', hidden: false, enforce: true, description: 'x' },
].forEach(function (decl, i) {
    const back = D.schemaFrom(Object.assign({ key: 'k' }, D.valuesFrom(decl, 'k')));
    // Compared canonically: the form writes its fields in the order a reader wants
    // them, which need not be the order the file had, and key order is not a value
    // (decision 007). What has to survive is what it says.
    eq('declaration ' + i + ' survives a trip through the form',
        U.sameValue(back, decl), true);
});

// Editing keeps what the form does not own. A map declaration carries its nested
// `properties`, and a keyword the form has no field for -- `optional` -- is still
// part of what the file says; the form rewrites the fields it asks about and lays
// them over the rest. Without this, editing a description dropped every declared
// key inside the map.
{
    const existing = {
        type: 'object',
        description: 'old',
        optional: 1,
        properties: { host: { type: 'string' }, port: { type: 'integer' } },
    };
    const edited = D.schemaFrom(Object.assign({}, D.valuesFrom(existing, 'spec'), { description: 'new' }));
    const merged = D.merged(existing, edited);
    eq('the edited field changed', merged.description, 'new');
    eq('nested declarations survive an edit of their parent', merged.properties, existing.properties);
    eq('and so does a keyword the form does not show', merged.optional, 1);
    eq('a field the form owns and now leaves empty is gone, not kept',
        D.merged({ type: 'string', description: 'x' }, { type: 'string' }).description, undefined);
    eq('the form keys come first, the kept ones after', Object.keys(merged), ['type', 'description', 'optional', 'properties']);
}
// Members of an enum are typed like the key: an integer key's values must not come
// back as strings after a trip through the form.
eq('an integer enum stays integers', D.schemaFrom({ type: 'integer', enum: '80, 443' }).enum, [80, 443]);
eq('a string enum stays strings', D.schemaFrom({ type: 'string', enum: '80, 443' }).enum, ['80', '443']);
eq('... and an integer declaration survives the round trip',
    U.canonical(D.schemaFrom(Object.assign({ key: 'k' }, D.valuesFrom({ type: 'integer', enum: [80, 443] }, 'k')))),
    U.canonical({ type: 'integer', enum: [80, 443] }));

// The two flags are three-state in both directions: absent means inherit, and
// `false` is a statement that has to come back as `false` rather than as absent.
eq('an absent flag reads as inherit',
    [D.valuesFrom({ type: 'string' }, 'k').hidden, D.valuesFrom({ type: 'string' }, 'k').enforce],
    ['inherit', 'inherit']);
eq('a false flag reads as false, not inherit',
    D.valuesFrom({ type: 'string', hidden: false }, 'k').hidden, 'false');
eq('the key is carried so the form can show it', D.valuesFrom({ type: 'string' }, 'host').key, 'host');

// Which rows are declarations, and which only look like one.
eq('a top-level declaration', U.declaredKeyAt('schema.properties.host'), 'host');
eq('a nested one', U.declaredKeyAt('schema.properties.spec.properties.host'), 'host');
eq('the properties map itself is not one', U.declaredKeyAt('schema.properties'), null);
eq('nor is the schema node', U.declaredKeyAt('schema'), null);
eq('nor a key inside a declaration', U.declaredKeyAt('schema.properties.host.format'), null);
eq('nor anything outside the schema', U.declaredKeyAt('selector.tag'), null);
eq('hidden writes true', D.schemaFrom({ type: 'string', hidden: 'true' }), { type: 'string', hidden: true });
eq('shown writes false, which is not the same as unset',
    D.schemaFrom({ type: 'string', hidden: 'false' }), { type: 'string', hidden: false });
eq('enforced writes true', D.schemaFrom({ type: 'string', enforce: 'true' }), { type: 'string', enforce: true });
eq('advisory writes false', D.schemaFrom({ type: 'string', enforce: 'false' }), { type: 'string', enforce: false });
eq('both at once', D.schemaFrom({ type: 'integer', hidden: 'true', enforce: 'false' }),
    { type: 'integer', hidden: true, enforce: false });
eq('an enum is a list, not a string', D.schemaFrom({ type: 'string', enum: 'always, no ,unless-stopped' }),
    { type: 'string', enum: ['always', 'no', 'unless-stopped'] });
// A range on a string, or a format on a number, would be a declaration nothing reads.
eq('a range belongs to a number', D.schemaFrom({ type: 'string', minimum: '1', maximum: '9' }), { type: 'string' });
eq('a format belongs to a string', D.schemaFrom({ type: 'integer', format: 'ip' }), { type: 'integer' });
// "No format" is the sentinel `none`, never `''` (the `KeyValue-1` bug; see the
// Declare Key form's format combobox in pve-meta-tree.js), so it must not land.
eq('the none sentinel is not a format', D.schemaFrom({ type: 'string', format: 'none' }), { type: 'string' });
eq('a real format still lands', D.schemaFrom({ type: 'string', format: 'ip' }), { type: 'string', format: 'ip' });
eq('multiline is a string thing too', D.schemaFrom({ type: 'string', multiline: true }), { type: 'string', multiline: 1 });
// An empty field is left out entirely: a schema full of nulls describes nothing, and
// the server's lint refuses null values anyway.
eq('empty fields are omitted', D.schemaFrom({ type: 'string', description: '', default: '', enum: '' }), { type: 'string' });
// A boolean default comes from a list, not a text box: `parseValue` reads truth the
// way the row editor's checkbox writes it, so "True" or "yes" typed into a field would
// have been stored as `false` -- the opposite of what was meant, in a cluster-wide file.
eq('a boolean default comes from the list', D.schemaFrom({ type: 'boolean', defaultBool: 'true' }), { type: 'boolean', default: true });
eq('... and "false" means false', D.schemaFrom({ type: 'boolean', defaultBool: 'false' }), { type: 'boolean', default: false });
eq('... an unset one is left out', D.schemaFrom({ type: 'boolean', defaultBool: '' }), { type: 'boolean' });
eq(
    'a boolean never reads the text field',
    D.schemaFrom({ type: 'boolean', default: 'yes', defaultBool: '' }),
    { type: 'boolean' },
);
// A map has no default the editor would ever read: `addShape` stops at an object and
// walks into it. Writing one would be a declaration nothing consumes -- and a string.
eq('a map takes no default', D.schemaFrom({ type: 'object', default: '{}' }), { type: 'object' });
eq('an array default still parses as a list', D.schemaFrom({ type: 'array', default: 'a,b' }), { type: 'array', default: ['a', 'b'] });

console.log('\n--- the editor follows the value\'s shape, not a declaration ---');
// A map is nested YAML: Monaco, not a one-line field. This is the case that had no
// editor at all -- Edit was disabled, and double-click and Enter both bailed out.
eq('a map is edited as text', U.editorKind({ kind: 'map' }), 'text');
// An array of scalars reads and edits fine on one line; an array of maps does not.
eq('an array of scalars stays inline', U.editorKind({ kind: 'array', rawValue: ['lan', 'wan'] }), 'inline');
eq('an array of maps is text', U.editorKind({ kind: 'array', rawValue: [{ a: 1 }] }), 'text');
eq('a scalar is inline', U.editorKind({ kind: 'string', rawValue: 'ct200.example' }), 'inline');
eq('a number is inline', U.editorKind({ kind: 'number', rawValue: 8080 }), 'inline');
// A block string is a real leaf (an ssh key, a note) and needs a real editor.
eq(
    'a string with newlines gets a textarea',
    U.editorKind({ kind: 'string', rawValue: 'services:\n  web:\n    image: nginx\n' }),
    'multiline',
);
// ... and a declared-but-unset one has no newline to detect, which is the only thing
// the `multiline` schema extension exists for.
eq('a declared multiline row, still unset', U.editorKind({ kind: 'string', multiline: 1 }), 'multiline');
eq('no row, no editor', U.editorKind(null), 'none');

// The Value column summarises; the row's own valueText must stay exact, because that
// is what the editor opens on.
eq('one line is shown as it is', U.previewText('nginx'), 'nginx');
eq('a block shows its first line and the rest', U.previewText('a\nb\nc\n'), 'a (+2 lines)');
eq('a trailing newline is not a line', U.previewText('a\n'), 'a');
eq('an empty value is empty', U.previewText(undefined), '');

// A declared `multiline` reaches the row through the same path as `format`.
{
    const root = { key: '', path: '', children: {}, present: true, kind: 'map' };
    const ns = Shape.of([{ prefix: 'notes', selector: { all: true }, schema: {
        type: 'object',
        properties: { body: { type: 'string', multiline: 1, description: 'Free text' } },
    } }], []);
    panel.addShape.call(panel, root, ns);
    eq('multiline reaches the row', root.children.notes.children.body.multiline, true);
    eq('and so does the description', root.children.notes.children.body.grammarDescription, 'Free text');
}

console.log('\n--- S3: document keys colliding with Object.prototype members ---');
// `constructor`/`toString`/`hasOwnProperty` are ordinary, unreserved document
// keys (DESIGN §7) that must become ordinary rows, not resolve through the
// prototype chain to the page's global Object.
const protoDoc = { constructor: 'ctor-value', toString: 'tostring-value', hasOwnProperty: 'hop-value' };
const protoRoot = { key: '', path: '', children: Object.create(null), present: true, kind: 'map' };
panel.addData.call(panel, protoRoot, protoDoc);
eq('proto-named keys become rows', Object.keys(protoRoot.children).sort(), [
    'constructor',
    'hasOwnProperty',
    'toString',
]);
['constructor', 'toString', 'hasOwnProperty'].forEach((k) => {
    eq(`${k} row is a plain entry, not the global`, typeof protoRoot.children[k], 'object');
    eq(`${k} row present`, protoRoot.children[k].present, true);
    eq(`${k} row path`, protoRoot.children[k].path, k);
    eq(`${k} row value`, protoRoot.children[k].value, protoDoc[k]);
});
eq('global Object untouched', typeof Object.create, 'function');
// And they cross the ABI as keys, both ways.
eq('proto-named keys survive the core', Codec.parse(Codec.dump(protoDoc, 'yaml'), 'yaml'), protoDoc);

console.log(fails ? `\n${fails} FAILURE(S)` : '\nall passed');
process.exit(fails ? 1 : 0);
