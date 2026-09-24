// Offline smoke test of pve-meta-tree.js: the editor's helpers, the row builder,
// the write an edit produces and the text-editor markers -- through the real pve-meta
// core, loaded from the same `.wasm` the package ships. A minimal Ext/PVE shim is
// enough; none of this touches the DOM.
//
// The rules themselves (the YAML codec, key names, shadowing, the schema findings)
// are tested where they live, in Rust. What this suite shows is that the wasm build
// loads and answers, that the JavaScript objects over it (Codec, Shape) hand the
// right things in and out, and that the editor's own logic on top of them still does
// what it did.
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
    // The browser's, which the editor uses for the Monaco load timeout and the
    // annotation debounce; a fresh vm context has neither.
    setTimeout,
    clearTimeout,
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
        // The header installs one stylesheet rule for unset rows; record it
        // so the suite can see the rule without a DOM.
        util: { CSS: { createStyleSheet: (css, id) => ctx.__styles.push([id, css]) } },
        Msg: { alert: (title, msg) => ctx.__alerts.push([title, msg]) },
        window: { Window: {} },
        panel: { Panel: {} },
        button: { Segmented: {} },
    },
    Proxmox: { Utils: { format_boolean: (v) => (v ? 'Yes' : 'No') } },
    __defined: [],
    __alerts: [],
    __styles: [],
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
// A section that has to wait for something -- a load promise, a debounced timer --
// pushes a thunk here. They run in order after everything synchronous, before the
// summary, so the output stays readable.
const asyncSections = [];
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

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
eq('unset rows dim through one recorded stylesheet rule, not a fixed grey', ctx.__styles, [
    ['pve-meta-faded', '.pve-meta-faded { opacity: 0.55; }'],
]);
// The core is lazy, and `syncButtons` runs on render ahead of the first document
// read; the registry grid's dialogs can open before it too. This editor has
// shipped two lazy-load ordering bugs already (js-yaml's). Nothing below may throw.
{
    const Core = ctx.PVE.meta.Core;
    eq('the core reports itself unloaded', Core.loaded(), false);
    throws('a direct call says so instead of trapping', () => Core.call('abi'), 'not loaded');
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
    eq('a bad key name is refused now', typeof ctx.PVE.meta.Utils.keyPathError('bad key'), 'string');
    eq('a bad file name too', typeof ctx.PVE.meta.Utils.fileNameError('my file'), 'string');
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
    eq('the instance is fine afterwards', Core.call('parse', 'yaml', 'ok: 1\n'), { ok: 1 });
}
{
    const err = throws('a parse error is a CoreError, not a trap', () => Core.call('parse', 'yaml', 'a: 1\nb: [\n'), 'failed to parse');
    eq('... carrying the line the parser stopped on', err && err.line, 3);
    // (`instanceof Error` would be the vm context's own Error, not this realm's.)
    eq('... and it is a CoreError with a message', err instanceof ctx.PVE.meta.CoreError && typeof err.message === 'string', true);
    throws('an unknown function is an error', () => Core.call('no_such_function'), 'unknown function');
    throws('a bad argument is an error', () => Core.call('shape_governing', [], 'a b'), 'invalid path');
    eq('the instance is fine after errors', Core.call('parse', 'yaml', 'ok: 1\n'), { ok: 1 });
}

const U = ctx.PVE.meta.Utils;
const Codec = ctx.PVE.meta.Codec;
const Shape = ctx.PVE.meta.Shape;
const Markers = ctx.PVE.meta.Markers;

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

console.log('\n--- rows are editable iff the document is (DESIGN §4) ---');
// Nothing is computed per path: PVE's ACLs decide read/write for the whole
// document, and the editor just reflects that.
const editableForP = ctx.PVE.meta.TreePanel.editableFor;
eq('writable document', editableForP.call({ access: { read: 1, write: 1 } }), true);
eq('read-only document', editableForP.call({ access: { read: 1, write: 0 } }), false);
eq('no access object yet', editableForP.call({}), false);

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
// A flat array store's one field is `field1`; the list is markup, the member text.
eq('an enum member is encoded in the list',
    U.editorFor({ kind: 'string', enumValues: ['<img src=x>'] }).listConfig.getInnerTpl('field1'),
    '{field1:htmlEncode}');
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

console.log('\n--- selector text (the "Applies to" column) ---');
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
    const strict = new Shape([{ prefix: 't', selector: { all: 1 }, enforce: 1, schema: S }]);
    const lax = new Shape([{ prefix: 't', selector: { all: 1 }, schema: S }]);
    eq('an enforcing prefix flags its findings', strict.findings({ t: { port: 'x' } }), [{ path: 't.port', msg: 'expected integer', enforced: true }]);
    eq('an ordinary one does not', lax.findings({ t: { port: 'x' } }), [{ path: 't.port', msg: 'expected integer' }]);
    eq('Perl\'s 0 is not enforce', new Shape([{ prefix: 't', selector: { all: 1 }, enforce: 0, schema: S }]).findings({ t: { port: 'x' } })[0].enforced, undefined);
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
    ctx.PVE.meta.Monaco.showDiff = (cfg) => diffs.push(cfg);
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

    // The toggle must give back what it took. `Codec.render` only knows the text
    // the document was *loaded* as, and `same` ignores key order, so a reorder or a
    // `#` comment typed into the buffer compared equal to the loaded document and
    // was quietly replaced by it -- the concrete way "switching to JSON and back
    // loses my edits" happened. The switch out of YAML now stashes the exact text.
    const edited = '# mine\nb:   1\na: [y, x]\n';
    ed = editor(edited);
    const round = { editor: ed, lang: 'yaml', original: handWritten };
    round.lang = 'json';
    Buffer.render(round, Buffer.convert({ editor: ed, lang: 'yaml', original: handWritten }, 'json', btn));
    eq('the JSON side is the value, which is all JSON can hold', ed.value, Codec.dump({ b: 1, a: ['y', 'x'] }, 'json'));
    round.lang = 'yaml';
    Buffer.render(round, Buffer.convert({ editor: ed, lang: 'json', original: handWritten }, 'yaml', btn));
    eq('coming back gives the comment and the order back, exactly', ed.value, edited);

    // ... unless the JSON side was edited, in which case the value is what carries
    // over and the YAML layout it used to have is not that value's.
    round.lang = 'json';
    Buffer.render(round, Buffer.convert({ editor: ed, lang: 'yaml', original: handWritten }, 'json', btn));
    ed.value = Codec.dump({ b: 7, a: ['y', 'x'] }, 'json');
    round.lang = 'yaml';
    Buffer.render(round, Buffer.convert({ editor: ed, lang: 'json', original: handWritten }, 'yaml', btn));
    eq('an edited JSON buffer comes back as a dump of what it now says',
        ed.value, Codec.dump({ b: 7, a: ['y', 'x'] }, 'yaml'));
    eq('no alerts through any of that', alerts(), []);

    // diff
    Buffer.diff({ editor: editor('b: 2\n'), lang: 'yaml', original: handWritten }, 'the title');
    eq('diff shows the buffer against the baseline', diffs.pop(), { title: 'the title', original: handWritten, modified: 'b: 2\n', lang: 'yaml' });
    Buffer.diff({ editor: editor('{}'), lang: 'json', original: 'a: [\n' }, 't');
    eq('... falling back to YAML when the loaded text cannot be shown as JSON', diffs.pop().lang, 'yaml');
    delete ctx.window.monaco;
}

console.log('\n--- the core and Monaco are fetched under names that change with them ---');
{
    // pve-ext fingerprints the script, not what the script fetches for itself, so
    // `make js` writes in the core's content hash and Monaco's version: a browser
    // never pairs an upgraded script with a core or a tree it cached before.
    const M = ctx.PVE.meta;
    const hash = require('crypto').createHash('sha256').update(fs.readFileSync(WASM)).digest('hex');
    const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'), 'utf8'));
    eq('the core is named by the hash of the .wasm it was built with',
        M.Core.SRC, '/pve2/js/pve-meta-extjs/pve-meta-core-' + hash.slice(0, 8) + '.wasm');
    eq('Monaco\'s tree is named by its pinned version',
        M.Monaco.VS, '/pve2/js/pve-meta-extjs/monaco-' + pkg.dependencies['monaco-editor'] + '/vs');
}

console.log('\n--- Monaco loads next to ExtJS ---');
// Ext's enumerable `$isFunction` made Monaco's ESM-to-AMD interop throw inside its
// own chunk, where the loader's errback never sees it: the Text view masked itself
// "Loading..." for good.
// The marker lives on the editor's own realm's Function.prototype, so the probe runs there.
{
    const inCtx = (src) => vm.runInContext(src, ctx);
    const keys = () => inCtx('(function () { let out = []; for (let k in function () {}) { out.push(k); } return out; })()');
    inCtx('Function.prototype.$isFunction = true');
    eq('Ext leaves an enumerable marker on every function', keys(), ['$isFunction']);
    ctx.PVE.meta.Monaco.hideExtFunctionMarker();
    eq('... which Monaco no longer walks into', keys(), []);
    eq('... and which Ext still reads', inCtx('(function () {}).$isFunction'), true);
    inCtx('delete Function.prototype.$isFunction');
}

// A failed load used to be the answer for the rest of the session: `me.promise`
// was never reset, so one dropped request took Text mode with it until the page
// was reloaded. And a script that neither loads nor errors left the caller's mask
// up with nothing to say, which is what the timeout is for.
asyncSections.push(async function () {
    console.log('\n--- a failed load is this attempt\'s answer, not the session\'s ---');
    const M = ctx.PVE.meta;

    // The core, with itself detached for the length of the check.
    const attached = M.Core.exports;
    M.Core.exports = null;
    M.Core.promise = null;
    ctx.fetch = () => Promise.reject(new Error('offline'));
    let err = null;
    await M.Core.load().catch((e) => (err = e));
    eq('a core that did not load is an error', String(err), 'Error: offline');
    eq('... and the next caller gets to try again', M.Core.promise, null);
    delete ctx.fetch;
    M.Core.exports = attached;
    M.Core.promise = null;

    // Monaco, with just enough of a page for the loader to run against.
    delete ctx.window.monaco;
    let script = null;
    const listeners = [];
    ctx.window.location = { origin: 'https://pve.example:8006' };
    ctx.window.addEventListener = (name, fn) => listeners.push([name, fn]);
    ctx.window.removeEventListener = (name, fn) => listeners.splice(listeners.findIndex((l) => l[1] === fn), 1);
    ctx.document.createElement = () => (script = {});

    let failed = null;
    const first = M.Monaco.load().catch((e) => (failed = e));
    await tick();
    eq('the loader script is fetched from the tree the package ships',
        script.src, 'https://pve.example:8006' + M.Monaco.VS + '/loader.js');
    script.onerror();
    await first;
    eq('a Monaco that did not load is an error', String(failed).indexOf('failed to load') !== -1, true);
    eq('... and it is not cached either', M.Monaco.promise, null);
    eq('... with nothing left listening for its chunks', listeners.length, 0);

    // Nothing at all: no load event, no error event. The timer is the only way out.
    const timers = [];
    const realSetTimeout = ctx.setTimeout;
    ctx.setTimeout = (fn, ms) => timers.push([fn, ms]);
    let timedOut = null;
    const second = M.Monaco.load().catch((e) => (timedOut = e));
    await tick();
    eq('the load is given 30 seconds', timers[0][1], M.Monaco.TIMEOUT);
    timers[0][0]();
    await second;
    eq('... and says so when they pass', String(timedOut), 'Error: Timed out loading the text editor');
    eq('... without disabling Text mode for good', M.Monaco.promise, null);

    ctx.setTimeout = realSetTimeout;
    delete ctx.window.location;
    delete ctx.window.addEventListener;
    delete ctx.window.removeEventListener;
    ctx.document.createElement = () => ({});
});

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
const panelBase = {
    registryDoc: false,
    docId: '200',
    access: { read: 1, write: 1 },
    // Already resolved for this guest, as `GET /meta/prefixes?id=` returns it: the
    // selector has already been matched server-side, so a listed entry always
    // reaches this document.
    prefixes: [
        { prefix: 'traefik', selector: { tag: 'traefik' }, schema: TRAEFIK_SCHEMA },
        { prefix: 'netbird', selector: { all: true } },
    ],
};
// One partial TreePanel double: `panelBase`'s fields plus every real method P
// defines (never reimplemented here), with `overrides` layered on top for
// whatever one test differs on -- including a fake method, which wins over the
// real one since it is already on `p` before the real methods are added.
function panelWith(overrides) {
    const p = Object.assign({}, panelBase, overrides);
    Object.keys(P).forEach((k) => {
        if (typeof P[k] === 'function' && !(k in p)) {
            p[k] = P[k];
        }
    });
    return p;
}
const panel = panelWith({});

// Ext's TreeStore, just enough of it for what buildTree does to the nodes: the
// root is a node of its own, with the text Ext gives it ("Root", `defaultRootText`)
// and no parent, and a branch is as open as its config said.
function fakeTreeStore() {
    const asNodes = function (children, parent) {
        return (children || []).map(function (cfg) {
            const n = { data: cfg, parentNode: parent, open: !!cfg.expanded };
            n.childNodes = asNodes(cfg.children, n);
            n.isLeaf = () => !!cfg.leaf;
            n.isExpanded = () => n.open;
            n.collapse = () => (n.open = false);
            n.set = (k, v) => (n.data[k] = v);
            return n;
        });
    };
    const cascadeBy = function (fn) {
        const walk = (n) => {
            fn(n);
            n.childNodes.forEach(walk);
        };
        walk(this);
    };
    const store = {
        root: null,
        getRoot() { return this.root; },
        setRoot(cfg) {
            const r = {
                data: { text: 'Root' },
                parentNode: null,
                cascadeBy,
                isLeaf: () => false,
                isExpanded: () => !!cfg.expanded,
            };
            r.childNodes = asNodes(cfg.children, r);
            this.root = r;
        },
        // Every row below the root, by path, with whether it is open.
        branches() {
            const out = {};
            this.root.cascadeBy((n) => {
                if (n !== this.root && !n.isLeaf()) {
                    out[n.data.path] = n.isExpanded();
                }
            });
            return out;
        },
    };
    store.setRoot({ expanded: true, children: [] });
    return store;
}

const shape = panel.shapeFor('200');
// Most-specific first, then by name: the order the server lists in, and the one
// the Shape resolves in, whatever order the listing arrived in.
eq('the prefixes that reach this guest', shape.declared().map((n) => n.prefix), ['netbird', 'traefik']);

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
    const wrote = [];
    const sd = { sendEdit: (edit) => wrote.push(edit) };
    sd.setToDefault = P.setToDefault;
    const row = (data) => ({ data: Object.assign({ docId: '100', path: 'traefik.spec.port' }, data) });

    sd.setToDefault.call(sd, row({ present: false, defaultValue: 80 }));
    eq('an unset row still takes the default', wrote.pop(), {
        path: 'traefik.spec.port', op: 'set', value: 80,
    });

    sd.setToDefault.call(sd, row({ present: true, rawValue: 8080, defaultValue: 80 }));
    eq('a wrong value can be put back to the default', wrote.pop(), {
        path: 'traefik.spec.port', op: 'set', value: 80,
    });

    sd.setToDefault.call(sd, row({ present: true, rawValue: 80, defaultValue: 80 }));
    eq('a row already at its default writes nothing', wrote.length, 0);

    sd.setToDefault.call(sd, row({ present: false }));
    eq('a row with no default writes nothing', wrote.length, 0);

    // Structural values compare by content, not identity: the core's dump of each.
    sd.setToDefault.call(sd, row({ present: true, rawValue: ['a'], defaultValue: ['a'] }));
    eq('an equal list is already at its default', wrote.length, 0);
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

console.log('\n--- the document is read as YAML: the file\'s own order survives ---');
{
    // `GET ...?format=json` renders the document as a native Perl hash, which has
    // no order and no booleans. Order is not a value, but a write at the root view
    // sends the document back, and keeping the file's order is what stops that from
    // churning it.
    const text = 'zebra: 1\nalpha: 2\nmiddle:\n  z: 1\n  a: 2\n';
    eq('parse keeps the document order', Object.keys(Codec.parse(text, 'yaml')), ['zebra', 'alpha', 'middle']);
    eq('... at every level', Object.keys(Codec.parse(text, 'yaml').middle), ['z', 'a']);
    eq('... and the dump preserves it', Codec.dump(Codec.parse(text, 'yaml'), 'yaml'), text);
}

console.log('\n--- a registry file that did not load is still a row ---');
{
    const G = ctx.PVE.meta.RegistryGrid.statics;
    // A prefix that stops parsing must still be listed, with its error --
    // silently ceasing to exist is the one failure mode with no symptom
    // (docs/decisions/003-registry-files-are-documents.md).
    const pfx = G.rowsFrom([
        { prefix: 'good', selector: { all: true }, origin: 'cluster' },
        { prefix: 'broken', origin: 'packaged', error: 'mapping values are not allowed here' },
    ]);
    eq('both files are rows', pfx.map((r) => r.name), ['good', 'broken']);
    eq('the failure keeps its error', pfx[1].error, 'mapping values are not allowed here');
    eq('... and says so where it would say what it does', pfx[1].description, pfx[1].error);
    eq('... and is still addressable, which is how it gets repaired', pfx[1].id, 'prefixes/broken');
    // The two flags as the grid shows them, in Perl's spelling and in JSON's.
    const flagged = G.rowsFrom([
        { prefix: 'p', selector: { all: true }, enforce: 1, hidden: true },
        { prefix: 'q', selector: { all: true }, enforce: '0', hidden: 0 },
    ]);
    eq('enforced and hidden read as yes', [flagged[0].enforce, flagged[0].hidden], ['yes', 'yes']);
    eq('... and 0 as nothing', [flagged[1].enforce, flagged[1].hidden], ['', '']);
    eq('... and keeps the origin, which is where to look for it', pfx[1].origin, 'packaged');
    eq('a file that loaded carries no error', pfx[0].error, undefined);

    // The safety half: a file that did not load must never describe anything. The
    // listing carries it; the Shape drops it.
    const p2 = panelWith({
        prefixes: [
            { prefix: 'netbird', selector: { all: true } },
            { prefix: 'broken', selector: { all: true }, error: 'nope' },
        ],
    });
    eq('a failed prefix reaches no guest, even carrying a selector', p2.shapeFor('200').declared().map((n) => n.prefix), ['netbird']);
}

console.log('\n--- a hidden declaration decorates a row, it never creates one ---');
{
    // A vocabulary the size of Traefik's is mostly keys nobody sets on a given
    // guest, and every one of them as a greyed row buries what the guest says.
    // `hidden` suppresses the offered row. It must not suppress data: a hidden key that
    // IS set still gets its row, its type and its default.
    const d = panelWith({
        docId: '100',
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
    const root = d.documentEntries.call(d);
    const routers = root.children.t.children.routers;

    eq('a shown declaration still gets its declared-but-unset row', root.children.t.children.host.present, false);
    eq('a hidden subtree is still a row, because the document has one', !!routers, true);
    eq('a hidden key that is set keeps its row', routers.children.rule.present, true);
    eq('... and is still typed by its schema', routers.children.rule.kind, 'string');
    eq('a hidden key that is not set has no row', routers.children.timeout, undefined);
    eq('an explicitly shown key inside a hidden subtree is offered',
        [!!routers.children.entrypoint, routers.children.entrypoint.present], [true, false]);
    eq('... with its default', routers.children.entrypoint.defaultValue, 'web');

    // Nothing stored under the hidden subtree at all: the shown key still creates
    // the rows above it, and the subtree's own row -- which its hidden declaration
    // would not have created -- is still decorated by it once it exists.
    d.docState[100].data = { t: {} };
    const bare = d.documentEntries.call(d).children.t.children.routers;
    eq('a shown child creates its hidden parent\'s row', [!!bare, bare.present], [true, false]);
    eq('... and the parent is still typed as the map its declaration says', bare.kind, 'map');
    eq('... offering only the shown child', Object.keys(bare.children), ['entrypoint']);
}

console.log('\n--- a prefix is a declaration, with or without a schema ---');
{
    // A prefix is itself a statement about the document: something of mine
    // lives at this key. It earns a row on that alone; it just has less to say than
    // a schema'd one.
    const doc = (data) => {
        const d = panelWith({ docId: '100', docState: { 100: { digest: 'x', data: data } } });
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
]);

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
eq('a prefix with no schema has none to offer', Shape.of([{ prefix: 'netbird', selector: { all: true } }]).hasSchema(), false);
eq('... but is still declared', Shape.of([{ prefix: 'netbird', selector: { all: true } }]).declared().length, 1);

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
// Monaco renders a hover as Markdown; a schema's description is plain text.
eq('a hover is escaped for Markdown, so it shows as written',
    Markers.hoverMarkdown('host__ and *port* [x](javascript:y) <b> 1..2'),
    'host\\_\\_ and \\*port\\* \\[x\\]\\(javascript:y\\) \\<b\\> 1\\.\\.2');

console.log('\n--- a hover belongs to the model, not to the first panel that asked ---');
{
    // Monaco registers a hover provider per *language*, so the one this editor
    // installs is shared by every buffer on the page. It used to close over the
    // panel that happened to open Text first: a second panel got no hovers at all,
    // and once the first was destroyed, neither did anything else.
    const registered = [];
    const model = (lines) => ({
        getLineCount: () => lines,
        getLineMaxColumn: () => 20,
    });
    const editorOn = (text, m) => ({
        getValue: () => text,
        getModel: () => m,
        dispose() {},
    });
    const fakeMonaco = {
        MarkerSeverity: { Error: 8, Warning: 4 },
        Range: function (line, from, endLine, to) {
            this.line = line;
            this.to = to;
        },
        editor: { setModelMarkers() {} },
        languages: { registerHoverProvider: (lang, provider) => registered.push([lang, provider]) },
    };
    ctx.monaco = fakeMonaco;
    ctx.window.monaco = fakeMonaco;
    ctx.PVE.meta.textHoverRegistered = false;

    const guestYaml = 'traefik:\n  spec:\n    host: a.example\n';
    const guestModel = model(3);
    const guest = panelWith({
        docId: '201',
        textLang: 'yaml',
        prefixes: [{ prefix: 'traefik', selector: { all: true }, schema: TRAEFIK_SCHEMA }],
        textEditor: editorOn(guestYaml, guestModel),
    });

    const otherYaml = 'netbird:\n  groups:\n  - lan\n';
    const otherModel = model(3);
    const other = panelWith({
        docId: '202',
        textLang: 'yaml',
        prefixes: [
            {
                prefix: 'netbird',
                selector: { all: true },
                schema: { type: 'object', properties: { groups: { type: 'array', description: 'Netbird groups' } } },
            },
        ],
        textEditor: editorOn(otherYaml, otherModel),
    });

    guest.annotateText();
    other.annotateText();
    eq('one provider for the page, however many panels', registered.length, 1);
    const hover = (m, line) => {
        const out = registered[0][1].provideHover(m, { lineNumber: line });
        return out && out.contents[0].value;
    };
    eq('the first panel\'s model is described by its own schema',
        hover(guestModel, 3), 'string \u00b7 Public host name');
    eq('and the second\'s by its own', hover(otherModel, 2), 'array \u00b7 Netbird groups');
    eq('a line nothing declares says nothing', hover(guestModel, 9), null);

    // And the map does not outlive the buffer.
    guest.disposeTextEditor();
    eq('a disposed buffer takes its hovers with it', hover(guestModel, 3), null);
    eq('... and leaves the other panel\'s alone', hover(otherModel, 2), 'array \u00b7 Netbird groups');
    other.disposeTextEditor();

    delete ctx.monaco;
    delete ctx.window.monaco;
}

console.log('\n--- nesting: the ROW builder must shadow too, not just the linter ---');
{
    const nsPanel = panelWith({});
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
eq('same ignores key order: order is not a value',
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
// An id is an address: the path it is served at, for every kind of document.
eq('a guest id', P.urlFor.call(P, '201'), '/meta/guests/201');
eq('a prefix id', P.urlFor.call(P, 'prefixes/homelab.docker'), '/meta/prefixes/homelab.docker');
eq('kind of a guest', P.docKind.call(P, '201'), 'guest');
eq('kind of a prefix', P.docKind.call(P, 'prefixes/traefik'), 'prefix');
eq('the title is the file name', P.docTitle.call(P, 'prefixes/homelab.docker'), 'homelab.docker');
eq('a guest id is its own title', P.docTitle.call(P, '201'), '201');
{
    // A guest tab asks for its own resolved set by id, and a registry document for
    // every file, as it is.
    const asked = (panel) => {
        let opts = null;
        P.loadPrefixes.call(Object.assign({ request: (o) => (opts = o), prefixParams: P.prefixParams }, panel), () => {});
        return opts.params;
    };
    eq('a guest tab lists its own prefix set by id', asked({ registryDoc: false, docId: '201' }), { id: '201' });
    eq('a registry document lists every file', asked({ registryDoc: true, docId: 'prefixes/ops' }), {});
    eq('a prefix id is its file name', P.registryId('gpu'), 'prefixes/gpu');
}

// A mask over the panel's own element covers the docked footer too, and the
// Tree|Text switch under it drops the click: the first one after a fresh render
// did nothing at all.
{
    let got = null;
    ctx.Proxmox.Utils.setErrorMask = (comp, msg) => (got = [comp.el, msg]);
    P.setMask.call({ el: 'panel', body: 'body' }, true);
    eq('the load mask covers the view', got, ['body', true]);
    P.setMask.call({ el: 'panel' }, 'boom');
    eq('... and the panel itself before there is one', got, ['panel', 'boom']);
    delete ctx.Proxmox.Utils.setErrorMask;
}

// Per-document digests. One shared field would have sent a prefix's digest with a
// write to another document, which is a 409 at best and the wrong document at worst.
{
    const panelM = panelWith({
        docId: 'prefixes/y',
        docState: { 'prefixes/y': { digest: 'aaa', data: { a: 1 } }, 'prefixes/x': { digest: 'bbb', data: {} } },
    });
    eq('each document keeps its own digest', panelM.digestOf('prefixes/x'), 'bbb');
    eq('and its own data', panelM.dataOf('prefixes/y'), { a: 1 });
    eq('an unknown document has no digest', panelM.digestOf('prefixes/nope'), '');
    eq('a row names its document', panelM.docOf({ data: { docId: 'prefixes/x' } }), 'prefixes/x');
    eq('no row means the default one', panelM.docOf(null), 'prefixes/y');
}

// What describes a registry document: its meta-schema, rooted at the document.
{
    const META = { type: 'object', properties: { selector: { type: 'object' } } };
    const panelG = panelWith({ registryDoc: true, schemas: { prefix: META } });
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
    const panelR = panelWith({
        registryDoc: true,
        docId: 'prefixes/x',
        docState: { 'prefixes/x': { digest: 'd', data: { selector: { tag: 'traefik' } } } },
        schemas: { prefix: META },
    });
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

console.log('\n--- the registry list ---');
{
    // `statics:` in the shim is a plain object; Ext hoists it onto the class.
    const G = ctx.PVE.meta.RegistryGrid.statics;
    const rows = G.rowsFrom([
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

    // An older API returns neither field; the list must still render.
    eq('a row with no origin is treated as the cluster\'s', G.originText(G.rowsFrom([{ prefix: 'x' }])[0]), 'cluster');

    // A file's per-node overrides ride along on the one row for it, as text --
    // there is no per-node row any more (DESIGN §3).
    const overrideRows = G.rowsFrom([
        { prefix: 'gpu', selector: { all: true }, nodes: { pve1: { hidden: true }, pve2: { enforce: true } } },
        { prefix: 'netbird', selector: { all: true } },
    ]);
    eq('override node names, comma-separated', overrideRows.map((r) => r.nodes), ['pve1, pve2', '']);

    // The buttons: any row opens, Remove is the registry answer's, and a packaged
    // file is never removable.
    const Grid = ctx.PVE.meta.RegistryGrid;
    const buttons = (row, may) => {
        const state = {};
        const grid = {
            access: { write: may ? 1 : 0 },
            getSelection: () => (row ? [{ data: row }] : []),
            down: (sel) => ({ setDisabled: (d) => (state[sel.slice(1)] = d) }),
        };
        Grid.syncButtons.call(grid);
        return [state.addBtn, state.editBtn, state.removeBtn];
    };
    const [clusterRow, pkgRow] = [{ origin: 'cluster' }, { origin: 'packaged' }];
    eq('with write on /: add, edit and remove a cluster file', buttons(clusterRow, true), [false, false, false]);
    eq('without it: a cluster file still opens, and is not removable', buttons(clusterRow, false), [true, false, true]);
    eq('a packaged file opens and is never removable', buttons(pkgRow, true), [false, false, true]);
    eq('no selection: nothing to edit or remove', buttons(null, true), [false, true, true]);

    // A listing that failed masks the grid with the reason; the next one that
    // works has to take it off again, or the grid stays behind the old message
    // with the rows it just loaded invisible underneath.
    {
        const masks = [];
        const setErrorMask = ctx.Proxmox.Utils.setErrorMask;
        ctx.Proxmox.Utils.setErrorMask = (comp, msg) => masks.push(msg);
        // As Ext does with `statics:`, for the one call `reload` makes.
        ctx.PVE.meta.RegistryGrid.rowsFrom = G.rowsFrom;
        const listing = { result: { data: [{ prefix: 'gpu', selector: { all: true } }] } };
        const grid = {
            access: { write: 1 },
            store: { setData: (rows) => (grid.rows = rows) },
            syncButtons() {},
            request(opts) {
                if (opts.url === '/meta/prefixes') {
                    if (grid.broken) {
                        opts.failure({ htmlStatus: 'connection error' });
                    } else {
                        opts.success(listing);
                    }
                }
            },
        };
        grid.broken = true;
        Grid.reload.call(grid);
        eq('a failed listing says so on the grid', masks, ['connection error']);
        grid.broken = false;
        Grid.reload.call(grid);
        eq('... and the next one that works clears it', masks, ['connection error', false]);
        eq('... showing what it loaded', grid.rows.map((r) => r.name), ['gpu']);
        ctx.Proxmox.Utils.setErrorMask = setErrorMask;
        if (!setErrorMask) {
            delete ctx.Proxmox.Utils.setErrorMask;
        }
        delete ctx.PVE.meta.RegistryGrid.rowsFrom;
    }

    // Remove asks, then deletes the row's own document.
    const sent = [];
    const [confirm, request] = [ctx.Ext.Msg.confirm, ctx.Proxmox.Utils.API2Request];
    ctx.Ext.Msg.confirm = (title, question, cb) => {
        sent.push(question);
        cb('yes');
    };
    ctx.Proxmox.Utils.API2Request = (opts) => sent.push([opts.method, opts.url]);
    Grid.removeOne.call({ reload() {} }, { data: { id: 'prefixes/homelab', origin: 'cluster', overrides: true, name: 'homelab' } });
    ctx.Ext.Msg.confirm = confirm;
    ctx.Proxmox.Utils.API2Request = request;
    eq('removing a cluster file deletes that document', sent[1], ['DELETE', '/meta/prefixes/homelab']);
    eq('... after saying what comes back', /packaged one takes over again/.test(sent[0]), true);
}

console.log('\n--- a key name is refused in the field, not after a round trip ---');
{
    // The charset itself is pinned in path.rs and registry.rs; one marshaling
    // spot-check per export, that the JS wrapper reads `why.char`/`why.segment`
    // and builds a message from them (DESIGN \u00a72 for the comment-key carve-out).
    eq('a plain key is fine', U.keyPathError('homelab'), null);
    eq('a bad key names the character and the segment',
        U.keyPathError('ok.bad key').indexOf('space') !== -1 && U.keyPathError('ok.bad key').indexOf('bad key') !== -1,
        true);
    eq('a comment key is fine', U.keyPathError('documented__'), null);
    eq('a file name is a prefix', U.fileNameError('homelab.docker'), null);
    eq('a bad file name is refused', typeof U.fileNameError('my file'), 'string');
}

console.log('\n--- one edit, one write (DESIGN §8) ---');
{
    // Every editor in this panel hands `writeFor` an edit and sends what comes back,
    // so this table is the whole write path. A `set` is `view::replace` at the edit's
    // own path -- which creates the maps above it, so a new dotted key needs no write
    // of its own to make room -- and a delete is `DELETE ?view=`, which takes the
    // key's note with it. Every write carries the digest and `comments=1`.
    const shown = (r) => [
        r.method,
        r.url,
        Object.keys(r.params || {}).sort().map((k) => k + '=' + r.params[k]).join(' '),
    ];
    [
        [
            'a row edit writes that key',
            { path: 'homelab.port', op: 'set', value: 8080 },
            false,
            ['PUT', '/meta/guests/201', 'comments=1 data=8080 digest=d0 mode=replace view=homelab.port'],
        ],
        [
            'a new key writes its own path, parents and all',
            { path: 'docker.compose.file', op: 'set', value: 'x' },
            false,
            ['PUT', '/meta/guests/201', 'comments=1 data="x" digest=d0 mode=replace view=docker.compose.file'],
        ],
        [
            'a list member is a write of the whole list',
            { path: 'netbird.groups', op: 'set', value: ['lan', 'wan'] },
            false,
            ['PUT', '/meta/guests/201', 'comments=1 data=["lan","wan"] digest=d0 mode=replace view=netbird.groups'],
        ],
        [
            'Set to Default writes the default',
            { path: 'traefik.spec.port', op: 'set', value: 80 },
            false,
            ['PUT', '/meta/guests/201', 'comments=1 data=80 digest=d0 mode=replace view=traefik.spec.port'],
        ],
        [
            'the root view is the document, named by leaving it out',
            { path: '', op: 'set', value: { a: 1 } },
            false,
            ['PUT', '/meta/guests/201', 'comments=1 data={"a":1} digest=d0 mode=replace'],
        ],
        [
            'a removal is a DELETE at that view',
            { path: 'netbird', op: 'delete' },
            false,
            ['DELETE', '/meta/guests/201?view=netbird&digest=d0', ''],
        ],
        [
            '"Save anyway" is the same write with force=1',
            { path: 'homelab.port', op: 'set', value: 8080 },
            true,
            ['PUT', '/meta/guests/201', 'comments=1 data=8080 digest=d0 force=1 mode=replace view=homelab.port'],
        ],
        [
            '... and the same removal',
            { path: 'netbird', op: 'delete' },
            true,
            ['DELETE', '/meta/guests/201?view=netbird&digest=d0&force=1', ''],
        ],
    ].forEach((c) => eq(c[0], shown(P.writeFor('201', c[1], 'd0', c[2])), c[3]));

    // A prefix file is addressed the same way: it is a document like any other.
    eq(
        'a prefix file is written like any other document',
        P.writeFor('prefixes/gpu', { path: 'selector', op: 'set', value: { all: true } }, '').url,
        '/meta/prefixes/gpu',
    );
}

console.log('\n--- Add does not overwrite a key that is there ---');
{
    // Add sends a `replace` at the typed path, so it used to replace whatever was
    // already there without a word -- while Remove asks before it drops a key. The
    // field says so instead, live, against the document the panel holds.
    const doc = { traefik: { spec: { host: 'a.example', port: 80 } }, netbird: { groups: ['lan'] } };
    const owner = { docId: '201', docState: { 201: { digest: 'd', data: doc } }, dataOf: P.dataOf };
    const keyField = function (cfg) {
        const w = Object.assign({}, ctx.PVE.meta.AddKeyWindow, { list: false, tree: owner }, cfg);
        return w.formItems().filter((f) => f.name === 'key')[0];
    };

    const inSpec = keyField({ parentPath: 'traefik.spec' });
    eq('a key that is not there is fine', inSpec.validator('scheme'), true);
    eq('one that is gets a field error, not a silent replace',
        inSpec.validator('host'), 'This key exists; use Edit to change it');
    eq('... a bad name is still refused first', typeof inSpec.validator('bad key'), 'string');

    const atRoot = keyField({ parentPath: '' });
    eq('the check follows the path that would be written', atRoot.validator('host'), true);
    eq('... including a dotted one', atRoot.validator('traefik.spec.port'),
        'This key exists; use Edit to change it');
    eq('... and a key holding a map counts as there', atRoot.validator('netbird'),
        'This key exists; use Edit to change it');

    // A list member has no name of its own, so there is nothing to collide with.
    const member = keyField({ parentPath: 'netbird.groups', list: true });
    eq('appending to a list is never a collision', member.validator(''), true);

    // And the window is given the document to check against.
    const created = [];
    const origCreate = ctx.Ext.create;
    ctx.Ext.create = (xtype, cfg) => {
        created.push([xtype, cfg]);
        return { on: () => {}, show: () => {} };
    };
    const adder = panelWith({ docId: '201', docState: owner.docState });
    adder.addKey('201', 'traefik.spec');
    eq('Add Key opens against the panel\'s own document',
        [created[0][1].parentPath, created[0][1].tree === adder], ['traefik.spec', true]);
    ctx.Ext.create = origCreate;
}

console.log('\n--- a popup closes when the write lands, not when the button is clicked ---');
{
    // Every one of these fired its event and closed in the same breath, so a 400
    // from the lint, a 403 or a dropped connection threw away what was typed --
    // for the subtree window, a page of YAML. The window now waits for the write.
    const win = function () {
        const w = { masks: [], closed: 0, isDestroyed: false, close() { this.closed++; } };
        w.body = { mask: () => w.masks.push('on'), unmask: () => w.masks.push('off') };
        return w;
    };

    const kept = win();
    ctx.PVE.meta.writeFromWindow(kept, (done) => (kept.done = done));
    eq('the body is masked while the write is in flight', kept.masks, ['on']);
    eq('... the window is not closed on the click', kept.closed, 0);
    kept.done(false);
    eq('a failed write unmasks and leaves it open', [kept.masks, kept.closed], [['on', 'off'], 0]);
    const gone = win();
    ctx.PVE.meta.writeFromWindow(gone, (done) => done(true));
    eq('a write that landed closes it', [gone.masks, gone.closed], [['on', 'off'], 1]);

    // Add Key: the edit it fires, and the callback it closes on.
    const add = Object.assign(win(), {
        parentPath: 'traefik',
        list: false,
        validForm: () => ({ getValues: () => ({ key: 'port', kind: 'number', value: '8080' }) }),
        fireEvent(name, path, value, done) {
            this.fired = [name, path, value];
            this.done = done;
        },
    });
    ctx.PVE.meta.AddKeyWindow.submit.call(add);
    eq('Add Key fires the edit at its own path', add.fired, ['addkey', 'traefik.port', 8080]);
    add.done(false);
    eq('... and a refused key leaves the form open to be corrected', add.closed, 0);
    add.done(true);
    eq('... closing once the key is stored', add.closed, 1);

    // The row editor.
    const edit = Object.assign(win(), {
        rec: { data: { path: 'traefik.spec.port', kind: 'number', present: true, rawValue: 80 } },
        validForm: () => ({}),
        down: () => ({ getValue: () => '8080' }),
        fireEvent(name, value, done) {
            this.fired = [name, value];
            this.done = done;
        },
    });
    ctx.PVE.meta.EditValueWindow.submit.call(edit);
    eq('the row editor fires the value', edit.fired, ['setvalue', 8080]);
    edit.done(false);
    eq('... and stays open on a failure', [edit.masks, edit.closed], [['on', 'off'], 0]);

    // "Edit selection as text": the one with a page of YAML to lose.
    const text = Object.assign(win(), {
        view: 'traefik',
        tree: {
            writeSubtree(view, value, done) {
                text.wrote = [view, value];
                text.done = done;
            },
        },
    });
    ctx.PVE.meta.TextWindow.apply.call(text, 'spec:\n  host: a.example\n', 'yaml');
    eq('the subtree window writes its view', text.wrote, ['traefik', { spec: { host: 'a.example' } }]);
    text.done(false);
    eq('... and keeps the buffer when the write is refused', [text.masks, text.closed], [['on', 'off'], 0]);
    text.done(true);
    eq('... closing when it is not', text.closed, 1);

    // New Prefix. (`statics:` is a plain object in the shim; Ext hoists it onto
    // the class, which is how `submit` reaches `planFrom`.)
    const New = ctx.PVE.meta.NewRegistryWindow;
    New.planFrom = New.statics.planFrom;
    const create = Object.assign(win(), {
        validForm: () => ({ getValues: () => ({ name: 'gpu', selector: 'all' }) }),
        fireEvent(name, plan, done) {
            this.fired = [name, plan.id];
            this.done = done;
        },
    });
    ctx.PVE.meta.NewRegistryWindow.submit.call(create);
    eq('New Prefix fires the plan', create.fired, ['create', 'prefixes/gpu']);
    create.done(false);
    eq('... and a name the server refuses leaves the form filled in', create.closed, 0);
    delete New.planFrom;
}

// A read-only caller could type into "Edit selection as text", press OK and get a
// 403 for their trouble; and a Monaco that failed to load masked the whole window,
// Cancel included, with no way out but Escape.
asyncSections.push(async function () {
    console.log('\n--- "Edit selection as text" says whether it can be written ---');
    const M = ctx.PVE.meta;
    const [realLoad, realCreate] = [M.Monaco.load, M.Monaco.create];
    const masks = [];
    ctx.Proxmox.Utils.setErrorMask = (comp, msg) => masks.push([comp.el, msg]);
    let created = null;
    M.Monaco.load = () => Promise.resolve();
    M.Monaco.create = (mount, value, options) => {
        created = { value: value, options: options };
        return { getValue: () => value, getModel: () => null, dispose() {} };
    };

    const open = async function (write) {
        const handlers = {};
        const ok = {
            disabled: true,
            setDisabled(d) { this.disabled = d; },
        };
        const win = Object.assign({}, M.TextWindow, {
            view: 'traefik',
            text: 'spec:\n  host: a.example\n',
            tree: { access: { read: 1, write: write ? 1 : 0 }, writeSubtree: () => (win.wrote = true) },
            body: 'the-body',
            el: 'the-window',
            callParent() {},
            on(name, fn) { handlers[name] = fn; },
            lookupReference: () => ({ getEl: () => ({ dom: 'the-mount' }) }),
            down: () => ok,
        });
        win.initComponent();
        handlers.afterrender();
        await tick();
        win.ok = ok;
        win.applyButton = win.bbar.filter((b) => b && b.itemId === 'metaApply')[0];
        return win;
    };

    const editing = await open(true);
    eq('a writable document is edited', editing.title, 'Edit selection as text: traefik');
    eq('... with OK offered, and live once there is a buffer',
        [editing.applyButton.hidden, editing.ok.disabled], [false, false]);
    eq('... and a buffer that can be typed into', created.options, { readOnly: false });

    const viewing = await open(false);
    eq('a document that may not be written is viewed', viewing.title, 'View selection as text: traefik');
    eq('... with no OK to press', viewing.applyButton.hidden, true);
    eq('... and a read-only buffer', created.options, { readOnly: true });
    eq('... which OK would not write even if it were pressed',
        (viewing.submit(), viewing.wrote), undefined);

    eq('the mask goes over the body, so Cancel stays clickable',
        masks.map((m) => m[0]), ['the-body', 'the-body', 'the-body', 'the-body']);

    M.Monaco.load = realLoad;
    M.Monaco.create = realCreate;
    delete ctx.Proxmox.Utils.setErrorMask;
});

console.log('\n--- what a write answers the editor that started it ---');
{
    // `onDone(ok)` is the whole contract: true once the server has it, false on
    // every failure the editor is left holding. A 422 answers nothing until the
    // "Save anyway" question is settled, so the window stays masked across it.
    const answers = [];
    const shown = [];
    const panelD = panelWith({ docId: '201', docState: { 201: { digest: 'd0', data: {} } }, reload() {} });
    const request = ctx.Proxmox.Utils.API2Request;
    const msgShow = ctx.Ext.Msg.show;
    let reply = 'no';
    ctx.Ext.Msg.show = function (cfg) {
        shown.push(cfg.title);
        cfg.fn(reply);
    };
    const edit = { path: 'a', op: 'set', value: 1 };

    ctx.Proxmox.Utils.API2Request = (opts) => opts.success({});
    panelD.sendEdit(edit, false, (ok) => answers.push(ok));
    eq('a write that landed says so', answers.splice(0), [true]);

    ctx.__alerts.splice(0);
    ctx.Proxmox.Utils.API2Request = (opts) =>
        opts.failure({ result: { status: 400 }, htmlStatus: 'not a key name' });
    panelD.sendEdit(edit, false, (ok) => answers.push(ok));
    eq('an error says so, once, and is shown', [answers.splice(0), ctx.__alerts.splice(0).length], [[false], 1]);

    ctx.Proxmox.Utils.API2Request = (opts) =>
        opts.failure({ result: { status: 422 }, htmlStatus: 'traefik.spec.port: expected integer' });
    panelD.sendEdit(edit, false, (ok) => answers.push(ok));
    eq('a 422 asks before it answers', shown.splice(0).length, 1);
    eq('... and Cancel is a failed write', answers.splice(0), [false]);

    // "Save anyway": the retry carries the same callback, so the window is
    // answered once, after the forced write -- not unmasked mid-question.
    reply = 'yes';
    let forced = null;
    let first = true;
    ctx.Proxmox.Utils.API2Request = function (opts) {
        if (first) {
            first = false;
            opts.failure({ result: { status: 422 }, htmlStatus: 'expected integer' });
            return;
        }
        forced = opts.params.force;
        opts.success({});
    };
    panelD.sendEdit(edit, false, (ok) => answers.push(ok));
    eq('Save anyway retries with force=1', forced, 1);
    eq('... and answers once, when that write lands', answers.splice(0), [true]);

    ctx.Ext.Msg.show = msgShow;
    ctx.Proxmox.Utils.API2Request = request;
    if (!request) {
        delete ctx.Proxmox.Utils.API2Request;
    }
}

console.log('\n--- acting on one member rewrites its list ---');
{
    // There is no path to `groups[1]`, so every action on a member is one write of
    // the whole list, at the list's own path.
    const sent = [];
    const stub = panelWith({
        docId: '201',
        docState: { 201: { digest: 'd', data: { netbird: { groups: ['lan', 'wan', 'dmz'] } } } },
        sendEdit: (edit) => sent.push(edit),
    });

    eq('the list as it stands', stub.listAt('netbird.groups'), ['lan', 'wan', 'dmz']);
    stub.writeListMember('netbird.groups', 1, 'wlan');
    eq('editing a member writes the list', sent.pop(), {
        path: 'netbird.groups',
        op: 'set',
        value: ['lan', 'wlan', 'dmz'],
    });
    stub.writeListMember('netbird.groups', 0, undefined);
    eq('removing a member writes the list without it', sent.pop().value, ['wan', 'dmz']);
    // An index that is not there changes nothing, rather than growing the list with
    // a hole in it.
    stub.writeListMember('netbird.groups', 9, 'nope');
    stub.writeListMember('netbird.groups', -1, 'nope');
    eq('an index that is not there is not a write', sent, []);
}

console.log('\n--- path helpers ---');
eq('parentPath', [U.parentPath('a.b.c'), U.parentPath('a'), U.parentPath('')], ['a.b', '', '']);

console.log('\n--- a list is a container, like a map ---');
{
    // The tree existed to make a document something you can look at and act on one
    // piece of. A list was the one shape that stayed a blob of JSON in a cell, for
    // no reason other than that it came second.
    const root = { key: '', path: '', children: {}, present: true, kind: 'map' };
    panel.addData.call(panel, root, {
        netbird: { groups: ['lan', 'wan'] },
        peers: [
            { host: 'a.example', port: 51820 },
            { host: 'b.example', port: 51821 },
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

    const peers = root.children.peers;
    eq('a list of maps too', Object.keys(peers.children).sort(), ['0', '1']);
    // A member with structure has no shape `itemSummary` recognises, so it falls
    // back to its JSON -- and carries its real value regardless.
    eq('an unrecognised shape falls back to JSON', peers.children['0'].value, '{"host":"a.example","port":51820}');
    eq('and the real thing rides along', peers.children['0'].rawItem.port, 51820);
    eq('an empty list has no members', Object.keys(root.children.empty.children), []);

    // The summary is presentation only, and describes nothing and constrains nothing.
    eq('an unknown shape is still legible', U.itemSummary({ a: 1 }), '{"a":1}');
    eq('a scalar member is itself', U.itemSummary('lan'), 'lan');
}

console.log('\n--- creating a registry file: the least that parses ---');
{
    const plan = (v) => ctx.PVE.meta.NewRegistryWindow.statics.planFrom(v);
    eq('a prefix with an "all" selector', plan({ name: 'x', selector: 'all' }).content, { selector: { all: true } });
    eq('it writes the cluster file named after the prefix', plan({ name: 'gpu', selector: 'all' }).id, 'prefixes/gpu');
    eq('a prefix with a tag selector', plan({ name: 'x', selector: 'tag', tag: 'web' }).content, { selector: { tag: 'web' } });
    eq(
        'a description when there is one',
        plan({ name: 'x', selector: 'all', description: 'Home' }).content,
        { description: 'Home', selector: { all: true } },
    );
}

console.log('\n--- reloading must not fold the tree up ---');
{
    // The key that survives a reload. A document row and a group row both live at the
    // empty path, and two group rows have no document at all, so keying on
    // (docId, path) collided: collapsing one group came back expanded on the next
    // reload because the other had won the shared key.
    const key = (n) => (n.data.docId || '') + '\u0000' + n.data.path + '\u0000' + (n.data.key || '');
    const groupA = { data: { docId: null, path: '', key: 'A' } };
    const groupB = { data: { docId: null, path: '', key: 'B' } };
    const dcRoot = { data: { docId: 'prefixes/homelab', path: '', key: 'prefixes/homelab' } };
    const nsRoot = { data: { docId: 'prefixes/gpu', path: '', key: 'prefixes/gpu' } };
    const same = { data: { docId: 'prefixes/homelab', path: 'selector', key: 'selector' } };
    const other = { data: { docId: 'prefixes/gpu', path: 'selector', key: 'selector' } };
    const keys = [groupA, groupB, dcRoot, nsRoot, same, other].map(key);
    eq('every row has a key of its own', new Set(keys).size, keys.length);
}

console.log('\n--- a write must not lose the selected row ---');
{
    // Every write rebuilds the tree, and the rebuilt tree had no selection at all:
    // after Edit, Add or Set to Default the toolbar went dead and the row had to be
    // hunted down again.
    const store = fakeTreeStore();
    let sel = [];
    const panelS = panelWith({
        docId: '201',
        prefixes: [],
        docState: { 201: { digest: 'd', data: { traefik: { spec: { host: 'a.example', port: 80 } } } } },
        store: store,
        tree: { getSelection: () => sel, setSelection: (rec) => (sel = [rec]) },
        syncButtons() {},
    });
    const find = (path) => {
        let hit = null;
        store.getRoot().cascadeBy((n) => {
            if (n.data.path === path) {
                hit = hit || n;
            }
        });
        return hit;
    };

    panelS.buildTree();
    eq('nothing selected, nothing to put back', panelS.getSelection(), []);

    sel = [find('traefik.spec.host')];
    panelS.buildTree();
    eq('the row a write touched is selected again, on the new node',
        [panelS.getSelection()[0].data.path, panelS.getSelection()[0] === sel[0]],
        ['traefik.spec.host', true]);

    // A Remove: the row is gone, so its parent takes the selection rather than
    // leaving the toolbar pointing at nothing.
    panelS.docState['201'].data = { traefik: { spec: { port: 80 } } };
    panelS.buildTree();
    eq('a removed row leaves its parent selected', panelS.getSelection()[0].data.path, 'traefik.spec');

    // And a selection in another document is not answered by a row of this one.
    eq('the key is the document, the path and the key',
        panelS.rowKey({ data: { docId: '201', path: 'a', key: 'a' } }) ===
            panelS.rowKey({ data: { docId: 'prefixes/x', path: 'a', key: 'a' } }),
        false);
    sel = [{ data: { docId: 'prefixes/x', path: 'traefik.spec.host', key: 'host' }, parentNode: null }];
    panelS.buildTree();
    eq('... so a row of another document matches nothing here',
        panelS.getSelection()[0].data.docId, 'prefixes/x');
}

console.log('\n--- the tree opens expanded, and a rebuild keeps what was folded ---');
{
    // The first build found Ext's hidden root, text "Root", and took it for a row
    // of an old tree in which nothing was expanded -- so every branch of a freshly
    // opened tab came up collapsed. headless-check calls expandAll, and the stub
    // store here used to have a root without a text, so nothing noticed.
    const store = fakeTreeStore();
    let sel = [];
    const panelE = panelWith({
        docId: '201',
        prefixes: [],
        docState: { 201: { digest: 'd', data: { a: { b: { c: 1 } }, m: { x: 1 }, top: 1 } } },
        store: store,
        tree: { getSelection: () => sel, setSelection: (rec) => (sel = [rec]) },
        syncButtons() {},
    });
    const find = (path) => {
        let hit = null;
        store.getRoot().cascadeBy((n) => (hit = hit || (n.data.path === path ? n : null)));
        return hit;
    };
    panelE.buildTree();
    eq('the first build leaves every branch open', store.branches(), { a: true, 'a.b': true, m: true });
    find('m').collapse();
    panelE.buildTree();
    eq('a rebuild keeps a folded branch folded, and the rest open', store.branches(), { a: true, 'a.b': true, m: false });

    // A Remove of a top-level row: its parent is the hidden root, which is not a
    // row to put the selection on.
    sel = [find('top')];
    panelE.docState['201'].data = { a: { b: { c: 1 } }, m: { x: 1 } };
    panelE.buildTree();
    eq('removing a top-level row does not select the hidden root', sel[0] === store.getRoot(), false);
}

console.log('\n--- the tree marks a row its schema refuses ---');
{
    // Same rule, same function -- `shapeFor` is shared by both callers so they
    // cannot answer differently.
    const SCHEMA = {
        type: 'object',
        properties: { port: { type: 'integer', minimum: 1, maximum: 65535 } },
    };
    const panelF = panelWith({
        registryDoc: false,
        docId: '201',
        docState: { 201: { digest: 'd', data: { docker: { port: 70000, host: 'ok' } } } },
        prefixes: [{ prefix: 'docker', selector: { all: true }, schema: SCHEMA }],
    });
    const found = panelF.findingsFor();
    eq('the out-of-range row is marked', found['docker.port'], 'must be at most 65535');
    eq('a row that fits is not', found['docker.host'], undefined);

    // A registry document is linted by its meta-schema through the same call.
    const panelR = panelWith({
        registryDoc: true,
        docId: 'prefixes/x',
        docState: { 'prefixes/x': { digest: 'd', data: { description: 5 } } },
        schemas: { prefix: { type: 'object', properties: { description: { type: 'string' } } } },
    });
    eq(
        'a prefix file is marked against the meta-schema',
        panelR.findingsFor().description,
        'expected string',
    );
}

console.log('\n--- "Declare Key" opens schema.properties as text ---');
{
    // There is no form of its own any more: the toolbar action opens the same
    // subtree text editor everything else stages through, on `schema.properties`.
    const created = [];
    const origCreate = ctx.Ext.create;
    ctx.Ext.create = (xtype, cfg) => {
        created.push([xtype, cfg]);
        return { on: () => {}, show: () => {} };
    };
    const withProps = panelWith({
        docId: 'prefixes/homelab',
        docState: {
            'prefixes/homelab': { digest: 'd', data: { schema: { properties: { host: { type: 'string' } } } } },
        },
    });
    withProps.declareKey({ data: { docId: withProps.docId } });
    eq('opens the text window', created[0][0], 'PVE.meta.TextWindow');
    eq('... on schema.properties', created[0][1].view, 'schema.properties');
    eq('... with what is already declared', Codec.parse(created[0][1].text, 'yaml'), { host: { type: 'string' } });
    // OK writes through the panel, so a document id on the window would be one the
    // write ignores -- a registry file edited from another document's panel.
    eq('... and no document of its own', created[0][1].docId, undefined);

    created.length = 0;
    const empty = Object.assign({}, withProps, {
        docState: { 'prefixes/homelab': { digest: 'd', data: {} } },
    });
    empty.declareKey({ data: { docId: empty.docId } });
    eq('an empty map gets a hint, not a blank buffer', Object.keys(Codec.parse(created[0][1].text, 'yaml')).length > 0, true);

    created.length = 0;
    withProps.declareKey(null);
    eq('nothing to declare on, nothing opens', created.length, 0);
    ctx.Ext.create = origCreate;
}

console.log('\n--- the panel takes its editors with it, and asks one question once ---');
{
    // A modal that outlives its panel edits a document nothing will write: its OK
    // goes through the panel, and PVE.meta.request drops the answer once the panel
    // is destroyed. They are tracked the way the text window already was.
    const opened = [];
    const origCreate = ctx.Ext.create;
    ctx.Ext.create = function (xtype, cfg) {
        const win = {
            xtype: xtype,
            closed: 0,
            handlers: {},
            on(name, fn) { this.handlers[name] = fn; },
            show() {},
            close() {
                this.closed++;
                this.handlers.destroy();
            },
        };
        opened.push(win);
        return win;
    };
    const host = panelWith({ docId: '201', docState: { 201: { digest: 'd', data: {} } } });
    host.addKey('201', '');
    host.addKey('201', 'traefik');
    eq('every editor it opens is tracked', host.editors.length, 2);
    opened[0].close();
    eq('... and one that closes on its own is forgotten', host.editors.length, 1);
    host.textWindow = { closed: 0, close() { this.closed++; } };
    const textWindow = host.textWindow;
    host.closeEditors();
    eq('destroying the panel closes what is left', [opened[1].closed, textWindow.closed], [1, 1]);
    eq('... and the editing flag is down again', host.editing, false);
    ctx.Ext.create = origCreate;

    // One question, one answer: syncAccessLabel calls syncFooter, and syncButtons
    // called it a second time on its own.
    let footers = 0;
    const modeBtn = { items: { getAt: () => ({ setDisabled() {}, setTooltip() {} }) } };
    const stub = { setDisabled() {}, setHidden() {}, setText() {}, setVisible() {} };
    const counted = panelWith({
        docId: '201',
        docState: { 201: { digest: 'd', data: {} } },
        access: { read: 1, write: 1 },
        mode: 'tree',
        tree: { getSelection: () => [] },
        down: (sel) => (sel === '#modeBtn' ? modeBtn : stub),
        syncFooter: () => footers++,
    });
    counted.syncButtons();
    eq('the footer is synced once per button sync', footers, 1);
}

console.log('\n--- the row toolbar is hidden in Text, not left there disabled ---');
{
    const comps = {};
    const comp = (id) =>
        (comps[id] = comps[id] || {
            setDisabled(d) { this.disabled = d; },
            setHidden(h) { this.hidden = h; },
            setVisible(v) { this.hidden = !v; },
            setText() {},
        });
    const modeBtn = { items: { getAt: () => ({ setDisabled() {}, setTooltip() {} }) } };
    const bar = panelWith({
        docId: '201',
        access: { read: 1, write: 1 },
        mode: 'tree',
        hasDefaults: false,
        tree: { getSelection: () => [] },
        down: (sel) => (sel === '#modeBtn' ? modeBtn : comp(sel.slice(1))),
        syncFooter() {},
    });
    const shown = () => Object.keys(comps).filter((id) => !comps[id].hidden).sort();
    bar.syncButtons();
    eq('in the tree: every row button, the separators and Reload; no default to offer, no Declare',
        shown(), ['addBtn', 'editBtn', 'metaToolbar', 'reloadBtn', 'removeBtn', 'rowSep', 'textSelBtn', 'textSep']);
    bar.mode = 'text';
    bar.syncButtons();
    eq('in Text: nothing, and with nothing to say the bar goes too', shown(), []);
    bar.access = { read: 1, write: 0 };
    bar.syncButtons();
    eq('... unless it says Read-only', shown(), ['accessText', 'metaToolbar']);
}

console.log('\n--- one rule for "did this change" ---');
{
    // The row editor compared `Ext.encode` of each value and `setToDefault` asked
    // the core's `same`: two rules for one question, and the string one made key
    // order part of the answer, which it is not (DESIGN §2).
    let fired = 0;
    const unchanged = Object.assign({}, ctx.PVE.meta.EditValueWindow, {
        rec: { data: { path: 'netbird.groups', kind: 'array', present: true, rawValue: ['lan', 'wan'] } },
        closed: 0,
        validForm: () => ({}),
        down: () => ({ getValue: () => 'lan, wan' }),
        fireEvent: () => fired++,
        close() { this.closed++; },
    });
    unchanged.submit();
    eq('a value that is the value already there is not a write', [fired, unchanged.closed], [0, 1]);
    const changed = Object.assign({}, unchanged, { closed: 0, down: () => ({ getValue: () => 'lan, dmz' }) });
    changed.submit();
    eq('... and one that differs is', fired, 1);

    // And the Text card's own dirty check is `Buffer.unchanged`, which knows the
    // buffer's language: the same document shown as JSON is not an edit.
    const card = panelWith({
        textLang: 'json',
        textOriginal: 'b:   1\na: [x, y]\n',
        textEditor: { getValue: () => Codec.dump({ b: 1, a: ['x', 'y'] }, 'json') },
    });
    eq('the loaded document, shown as JSON, is nothing to discard', card.textIsDirty(), false);
    card.textEditor = { getValue: () => '{"b": 2}' };
    eq('... an edit is', card.textIsDirty(), true);
    eq('no buffer at all, nothing to lose', panelWith({}).textIsDirty(), false);
}

console.log('\n--- the squiggles wait for a pause in the typing ---');
{
    // annotateText parses the whole buffer and asks the core for every finding;
    // running that on every keystroke is work nobody asked for, and the answer is
    // stale before it is drawn. Anything that acts on the buffer flushes it first.
    const timers = [];
    const [realSet, realClear] = [ctx.setTimeout, ctx.clearTimeout];
    ctx.setTimeout = (fn, ms) => timers.push([fn, ms]);
    ctx.clearTimeout = (id) => (timers[id - 1] = null);
    const diffs = [];
    const shownDiff = ctx.PVE.meta.Monaco.showDiff;
    ctx.PVE.meta.Monaco.showDiff = (cfg) => diffs.push(cfg);
    let annotated = 0;
    const typing = panelWith({
        docId: '201',
        docState: { 201: { digest: 'd', data: { a: 1 } } },
        textLang: 'yaml',
        textOriginal: 'a: 1\n',
        textEditor: { getValue: () => 'a: 1\n' },
        annotateText: () => annotated++,
    });

    typing.scheduleAnnotate();
    eq('a keystroke does not run the parser', annotated, 0);
    eq('... it asks again in 150 ms', timers[0][1], typing.ANNOTATE_DELAY);
    typing.scheduleAnnotate();
    eq('... and the next keystroke replaces that timer', [timers[0], timers.length], [null, 2]);
    timers[1][0]();
    eq('... one run when the typing stops', annotated, 1);

    typing.scheduleAnnotate();
    typing.showDiff();
    eq('Diff sees the buffer as it is now', annotated, 2);
    eq('... and is still a diff', diffs.length, 1);
    typing.scheduleAnnotate();
    typing.applyText();
    eq('and so does Apply', annotated, 3);

    ctx.PVE.meta.Monaco.showDiff = shownDiff;
    ctx.setTimeout = realSet;
    ctx.clearTimeout = realClear;
}

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
    } }]);
    panel.addShape.call(panel, root, ns);
    eq('multiline reaches the row', root.children.notes.children.body.multiline, true);
    eq('and so does the description', root.children.notes.children.body.grammarDescription, 'Free text');
}

console.log('\n--- the editor reads and writes the notes: every document request says comments=1 ---');
// The server leaves comment keys out of a read, and keeps the stored ones through a
// replace, unless asked (DESIGN §2, §7). This editor shows them as the description
// column and edits them, so it asks on every read and every write of a document.
{
    const sent = [];
    const T = ctx.PVE.meta.TreePanel;
    const fake = (extra) => Object.assign(
        {
            docId: '201',
            docState: { 201: { digest: 'd0', data: { a: 1, a__: 'about a' } } },
            urlFor: T.urlFor,
            docParams: T.docParams,
            digestOf: T.digestOf,
            dataOf: T.dataOf,
            setDigest: T.setDigest,
            write: T.write,
            writeFor: T.writeFor,
            sendEdit: T.sendEdit,
            flushAnnotate: T.flushAnnotate,
            cancelAnnotate: T.cancelAnnotate,
            clearParseErrorIfSound: T.clearParseErrorIfSound,
            request: (opts) => sent.push(Object.assign({ method: 'GET' }, opts)),
            submit: (opts) => sent.push(opts),
        },
        extra,
    );
    eq('docParams adds comments=1 and keeps the rest', T.docParams({ format: 'yaml' }), { comments: 1, format: 'yaml' });

    T.refreshText.call(fake({ textOriginal: '' }));
    let req = sent.pop();
    eq('the Text card reads with comments=1', [req.method, req.url, req.params], ['GET', '/meta/guests/201', { comments: 1, format: 'yaml' }]);

    T.sendEdit.call(fake({}), { path: 'a__', op: 'set', value: 'about a, edited' });
    req = sent.pop();
    eq('a row edit writes the note it names, comments=1',
        [req.method, req.url, req.params],
        ['PUT', '/meta/guests/201', { comments: 1, mode: 'replace', data: '"about a, edited"', digest: 'd0', view: 'a__' }]);

    T.applyText.call(fake({
        textEditor: { getValue: () => 'a: 3\na__: typed\n' },
        textBuffer: () => ({ editor: { getValue: () => 'a: 3\na__: typed\n' }, lang: 'yaml', original: 'a: 1\n' }),
        textLang: 'yaml',
    }));
    req = sent.pop();
    eq('the Text card\'s Apply sends the buffer, comments=1',
        [req.method, req.url, req.params],
        ['PUT', '/meta/guests/201', { comments: 1, mode: 'replace', digest: 'd0', text: 'a: 3\na__: typed\n' }]);
    eq('nothing else was sent', sent.length, 0);
}

console.log('\n--- a repaired document can go back to the tree ---');
{
    // A document that does not parse has no rows, so Text is the only view of it
    // and the only place it gets repaired. `docParseError` is what disables the
    // Tree segment, and only `reload` cleared it -- which returns early while the
    // mode is text, so a repaired document stayed text-only until the page was
    // reloaded. The read after an Apply is what knows better.
    const tree = {
        disabled: true,
        tooltip: null,
        setDisabled(d) { this.disabled = d; },
        setTooltip(t) { this.tooltip = t; },
    };
    const modeBtn = { items: { getAt: (i) => (i === 0 ? tree : { setDisabled() {} }) } };
    const reading = (text) => (opts) => opts.success({ result: { data: { digest: 'd', text: text } } });
    const panelP = panelWith({
        docId: '201',
        mode: 'text',
        textLang: 'yaml',
        docParseError: 'mapping values are not allowed here',
        docState: { 201: { digest: 'd0', data: {} } },
        down: (sel) => (sel === '#modeBtn' ? modeBtn : null),
        syncFooter() {},
        request: reading('a: 1\nb: [\n'),
    });

    panelP.refreshText();
    eq('text that still does not parse keeps the tree out of reach',
        [panelP.docParseError, tree.tooltip], ['mapping values are not allowed here', null]);

    panelP.request = reading('a: 1\n');
    panelP.refreshText();
    eq('a document that parses again clears the error', panelP.docParseError, '');
    eq('... and the Tree segment is live, with no tooltip to explain itself',
        [tree.disabled, tree.tooltip], [false, undefined]);
}

console.log('\n--- a 409 in Text mode keeps the buffer ---');
{
    // The buffer is unwritten work and the only copy of it. A 409 says the file
    // moved under it, which is a reason to show the difference, not to drop what
    // was typed on the floor and put the server's text there instead.
    const typed = '# mine\nb: 2\na: 1\n';
    const stored = 'a: 1\nc: 3\n';
    const editor = { value: typed, getValue() { return this.value; }, setValue(v) { this.value = v; } };
    const diffs = [];
    const shown = ctx.PVE.meta.Monaco.showDiff;
    ctx.PVE.meta.Monaco.showDiff = (cfg) => diffs.push(cfg);
    ctx.__alerts.splice(0);
    const panelT = panelWith({
        docId: '201',
        mode: 'text',
        textLang: 'yaml',
        textOriginal: 'a: 1\n',
        textEditor: editor,
        docState: { 201: { digest: 'd0', data: { a: 1 } } },
        // The re-read a conflict triggers: the document as somebody else left it.
        request: (opts) => opts.success({ result: { data: { digest: 'd9', text: stored } } }),
        annotateText: () => {},
    });
    ctx.Proxmox.Utils.API2Request = (opts) =>
        opts.failure({ result: { status: 409 }, htmlStatus: 'digest mismatch' });
    panelT.applyText();
    delete ctx.Proxmox.Utils.API2Request;
    ctx.PVE.meta.Monaco.showDiff = shown;

    eq('the buffer is still what was typed', editor.value, typed);
    eq('the digest is the one the next Apply needs', panelT.digestOf('201'), 'd9');
    eq('and the buffer is now compared against the new file', panelT.textOriginal, stored);
    eq('the conflict is reported', ctx.__alerts.splice(0), [['Conflict', 'digest mismatch']]);
    eq('... with the diff against what is in the file now', diffs.pop(), {
        title: '201',
        original: stored,
        modified: typed,
        lang: 'yaml',
    });
}

console.log('\n--- a 409 under an open editor re-reads around it, and the next OK goes through ---');
{
    // Since 9825441 a popup stays open until its write answers, and a 409 asked
    // `reload` to fetch the new digest -- which returns early under an editor. So
    // OK gave 409 for ever. Blindly refreshing the digest would be worse: the next
    // OK would write over the other writer's change without a word. The document
    // is re-read around the editor instead, and the value at the edit's own path
    // is compared before and after.
    const stored = (text) => (opts) => opts.success({ result: { data: { digest: 'd9', text: text } } });
    const openedWins = [];
    const origCreate = ctx.Ext.create;
    ctx.Ext.create = function (xtype, cfg) {
        const win = {
            xtype: xtype,
            cfg: cfg,
            handlers: {},
            on(name, fn) { this.handlers[name] = fn; },
            show() {},
            close() { this.handlers.destroy(); },
        };
        openedWins.push(win);
        return win;
    };
    // A panel whose `reload` is the real one, with the load chain cut at its
    // first request: what counts is whether the rows were asked for at all.
    const withEditor = function (read) {
        const p = panelWith({
            docId: '201',
            rendered: true,
            docState: { 201: { digest: 'd0', data: { a: 1, m: { x: 1 } } } },
            request: read,
            setMask() {},
            loads: 0,
            loadPrefixes() { this.loads++; },
        });
        p.openEditor('PVE.meta.EditValueWindow', { rec: { data: { path: 'a' } } }, 'setvalue', () => {});
        return p;
    };
    const answers = [];
    const sent = [];
    const conflictOnce = function (opts) {
        sent.push(opts);
        if (sent.length === 1) {
            opts.failure({ result: { status: 409 }, htmlStatus: 'digest mismatch' });
        } else {
            opts.success({});
        }
    };

    // The document changed elsewhere, but not at the edited path.
    ctx.__alerts.splice(0);
    let p = withEditor(stored('a: 1\nb: 2\nm:\n  x: 1\n'));
    ctx.Proxmox.Utils.API2Request = conflictOnce;
    p.sendEdit({ path: 'a', op: 'set', value: 5 }, false, (ok) => answers.push(ok));
    eq('the 409 answers the editor with a failure, so it stays open', answers.splice(0), [false]);
    eq('the digest is the one the next OK needs', p.digestOf('201'), 'd9');
    eq('... and the data too, without the rows being rebuilt under the editor',
        [p.dataOf('201').b, p.loads, p.reloadPending], [2, 0, true]);
    let alert = ctx.__alerts.splice(0)[0];
    eq('the alert says the document changed and the edit can go again as it is',
        [alert[0], alert[1].indexOf('digest mismatch') === 0, /still the one this editor opened on/.test(alert[1])],
        ['Conflict', true, true]);
    p.sendEdit({ path: 'a', op: 'set', value: 5 }, false, (ok) => answers.push(ok));
    eq('the second OK carries the fresh digest and lands', [sent[1].params.digest, answers.splice(0)], ['d9', [true]]);
    eq('the reload it earned is still owed while the editor is open', [p.loads, p.reloadPending], [0, true]);
    openedWins.pop().close();
    eq('... and paid once the editor closes', [p.loads, p.reloadPending, p.editing], [1, false, false]);

    // The edited value itself was changed underneath.
    sent.length = 0;
    ctx.__alerts.splice(0);
    p = withEditor(stored('a: 7\nm:\n  x: 1\n'));
    ctx.Proxmox.Utils.API2Request = conflictOnce;
    p.sendEdit({ path: 'a', op: 'set', value: 5 }, false, (ok) => answers.push(ok));
    alert = ctx.__alerts.splice(0)[0];
    eq('the alert names the value it opened on and the value there now',
        [alert[0], /it was 1 and is now 7/.test(alert[1]), /writes the value in this editor over the new one/.test(alert[1])],
        ['Conflict', true, true]);
    eq('... the editor is still open with the fresh digest', [answers.splice(0), p.editing, p.digestOf('201')], [[false], true, 'd9']);
    openedWins.pop().close();

    // A second 409 under the same editor: the first re-read moved the panel's copy
    // to a: 7, but the editor still opened on a: 1 -- that is what the value has to
    // be compared with, not with what the first conflict found.
    sent.length = 0;
    ctx.__alerts.splice(0);
    p = withEditor(stored('a: 7\nm:\n  x: 1\n'));
    ctx.Proxmox.Utils.API2Request = function (opts) {
        sent.push(opts);
        opts.failure({ result: { status: 409 }, htmlStatus: 'digest mismatch' });
    };
    p.sendEdit({ path: 'a', op: 'set', value: 5 }, false, () => {});
    ctx.__alerts.splice(0);
    p.request = stored('a: 7\nb: 3\nm:\n  x: 1\n');
    p.sendEdit({ path: 'a', op: 'set', value: 5 }, false, () => {});
    alert = ctx.__alerts.splice(0)[0];
    eq('a second 409 still compares with the value the editor opened on',
        [/it was 1 and is now 7/.test(alert[1]), /still the one this editor opened on/.test(alert[1])],
        [true, false]);
    openedWins.pop().close();
    eq('... which goes with the editor', p.openedOn, null);

    // Add Key: the key was not there, and now it is.
    sent.length = 0;
    ctx.__alerts.splice(0);
    p = withEditor(stored('a: 1\nm:\n  x: 1\nnew: taken\n'));
    ctx.Proxmox.Utils.API2Request = conflictOnce;
    p.sendEdit({ path: 'new', op: 'set', value: 'mine' }, false, () => {});
    eq('a key added by both sides is named as not set, then set',
        /it was not set and is now taken/.test(ctx.__alerts.splice(0)[0][1]), true);
    openedWins.pop().close();

    // No editor open (Set to Default, Remove): the reload and the message, as before.
    sent.length = 0;
    ctx.__alerts.splice(0);
    p = withEditor(stored('a: 7\n'));
    openedWins.pop().close();
    p.loads = 0;
    ctx.Proxmox.Utils.API2Request = conflictOnce;
    p.sendEdit({ path: 'a', op: 'set', value: 5 });
    eq('with nothing open a 409 reloads the tree and says so',
        [p.loads, ctx.__alerts.splice(0)], [1, [['Conflict', 'digest mismatch']]]);

    // The subtree text window: the same at its view, plus the diff against the
    // subtree as stored now, which is what the buffer is compared against from
    // here on. The buffer itself is left alone.
    const diffs = [];
    const shown = ctx.PVE.meta.Monaco.showDiff;
    ctx.PVE.meta.Monaco.showDiff = (cfg) => diffs.push(cfg);
    const typed = 'x: 1\ny: typed\n';
    const textWin = {
        view: 'm',
        lang: 'yaml',
        original: 'x: 1\n',
        editor: { value: typed, getValue() { return this.value; }, setValue(v) { this.value = v; } },
        buffer: ctx.PVE.meta.TextWindow.buffer,
        conflict: ctx.PVE.meta.TextWindow.conflict,
    };
    sent.length = 0;
    ctx.__alerts.splice(0);
    p = withEditor(stored('a: 1\nm:\n  x: 2\n'));
    openedWins.pop().close();
    p.loads = 0;
    p.textWindow = textWin;
    ctx.Proxmox.Utils.API2Request = conflictOnce;
    p.writeSubtree('m', { x: 1, y: 'typed' }, (ok) => answers.push(ok));
    alert = ctx.__alerts.splice(0)[0];
    eq('the text window\'s conflict is answered at its view, with the YAML\'s first lines',
        [answers.splice(0), /it was x: 1 and is now x: 2/.test(alert[1])], [[false], true]);
    eq('... the buffer is what was typed', textWin.editor.value, typed);
    eq('... compared against the subtree as stored now', textWin.original, 'x: 2\n');
    eq('... and the diff is the buffer against that', diffs.pop(), { title: 'm', original: 'x: 2\n', modified: typed, lang: 'yaml' });
    eq('... with the rows left for the close', [p.loads, p.reloadPending], [0, true]);
    p.writeSubtree('m', { x: 1, y: 'typed' }, (ok) => answers.push(ok));
    eq('the next OK carries the fresh digest', [sent[1].params.digest, answers.splice(0)], ['d9', [true]]);

    sent.length = 0;
    ctx.__alerts.splice(0);
    p.docState[201] = { digest: 'd0', data: { a: 1, m: { x: 1 } } };
    p.request = stored('a: 2\nm:\n  x: 1\n');
    textWin.original = 'x: 1\n';
    p.writeSubtree('m', { x: 1, y: 'typed' }, () => {});
    eq('a change elsewhere in the document opens no diff', [diffs.length, textWin.original], [0, 'x: 1\n']);
    eq('... and says the view is as it was', /still the one this editor opened on/.test(ctx.__alerts.splice(0)[0][1]), true);
    ctx.PVE.meta.Monaco.showDiff = shown;
    delete ctx.Proxmox.Utils.API2Request;

    // New Prefix: the empty digest matched nothing, so the name is taken. The
    // form stays open with its fields, and the message says that, not "digest
    // mismatch" about a document the form never read.
    ctx.__alerts.splice(0);
    openedWins.length = 0;
    const grid = { reloads: 0, reload() { this.reloads++; } };
    ctx.PVE.meta.RegistryGrid.createOne.call(grid);
    ctx.Proxmox.Utils.API2Request = (opts) => opts.failure({ result: { status: 409 }, htmlStatus: 'digest mismatch' });
    openedWins[0].handlers.create({ id: 'prefixes/homelab', file: 'homelab', content: {} }, (ok) => answers.push(ok));
    delete ctx.Proxmox.Utils.API2Request;
    alert = ctx.__alerts.splice(0)[0];
    eq('a taken name keeps the form open and says whose fault it is',
        [answers.splice(0), alert[0], /"homelab" already exists/.test(alert[1]), grid.reloads],
        [[false], 'Conflict', true, 1]);
    ctx.Ext.create = origCreate;
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

(async function () {
    for (const section of asyncSections) {
        await section();
    }
    console.log(fails ? `\n${fails} FAILURE(S)` : '\nall passed');
    process.exit(fails ? 1 : 0);
})();
