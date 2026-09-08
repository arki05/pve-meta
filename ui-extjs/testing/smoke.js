// Offline smoke test of the parts of pve-meta-tree.js that are pure JS: the
// YAML wrappers around the vendored js-yaml, the value helpers, and the
// entry-merge that builds the rows. A minimal Ext/PVE shim is enough - none of
// this touches the DOM.
const fs = require('fs');
const vm = require('vm');
const path = require('path');

// The same file the panel loads lazily in the browser (vendor/js-yaml.min.js is
// a UMD bundle, so `require` gets the exact build that ships in the package).
const jsyaml = require(path.join(__dirname, '..', 'vendor', 'js-yaml.min.js'));

const ctx = {
    console,
    window: { jsyaml },
    document: { createElement: () => ({}), head: { appendChild() {} } },
    Promise,
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
        apply: Object.assign,
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
        window: { Window: {} },
        panel: { Panel: {} },
        button: { Segmented: {} },
    },
    Proxmox: { Utils: { format_boolean: (v) => (v ? 'Yes' : 'No') } },
    __defined: [],
};
ctx.PVE = {};
vm.createContext(ctx);
vm.runInContext(
    fs.readFileSync(path.join(__dirname, '..', 'pve-meta-tree.js'), 'utf8'),
    ctx,
    { filename: 'pve-meta-tree.js' },
);

const U = ctx.PVE.meta.Utils;
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

console.log('--- classes defined ---');
eq('defined', ctx.__defined, [
    'PVE.meta.TreeModel',
    'PVE.meta.AddKeyWindow',
    'PVE.meta.EditValueWindow',
    'PVE.meta.TextWindow',
    'PVE.meta.TreePanel',
]);

console.log('\n--- covers / paths ---');
eq('covers exact', U.covers('traefik', 'traefik'), true);
eq('covers child', U.covers('traefik', 'traefik.spec.host'), true);
eq('covers sibling comment', U.covers('traefik', 'traefik__'), true);
eq('covers not prefix-of-name', U.covers('traefik', 'traefikx'), false);
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

console.log('\n--- the row editor field comes from the grammar first ---');
eq('editor enum', U.editorFor({ kind: 'string', enumValues: ['a'] }).xtype, 'combobox');
eq('editor boolean', U.editorFor({ kind: 'boolean' }).xtype, 'proxmoxcheckbox');
eq('editor number', U.editorFor({ kind: 'number' }).xtype, 'numberfield');
eq('editor array', U.editorFor({ kind: 'array' }).xtype, 'textfield');

// A grammar's `minimum`/`maximum` reach the number editor, and its `format` is
// resolved to the proxmoxlib vtype that already validates that shape (DESIGN §8).
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

console.log('\n--- row icons (DESIGN §8) ---');
eq('icons', ctx.PVE.meta.Icons, {
    map: 'fa fa-folder',
    mapExpanded: 'fa fa-folder-open',
    leaf: 'fa fa-file-text-o',
});

console.log('\n--- YAML: the vendored js-yaml, through the panel wrappers ---');
eq('vendored version', jsyaml.dump !== undefined && typeof jsyaml.load, 'function');
// The exact canonical dump the live store produced for guest 200: js-yaml must
// read what the server writes (the server stays the YAML authority).
const storeYaml =
    'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups__: asdf\n  groups:\n  - lan\n';
const storeDoc = {
    traefik: { spec: { host: 'ct200.example' } },
    netbird: { groups__: 'asdf', groups: ['lan'] },
};
eq('load the store dump', U.yamlLoad(storeYaml), storeDoc);
eq('round trip the store document', U.yamlLoad(U.yamlDump(storeDoc)), storeDoc);
eq('empty document is the empty map', U.yamlLoad(''), {});
// noRefs: a repeated subtree must not come back as an anchor/alias.
const shared = { a: 1 };
eq('no anchors', U.yamlDump({ x: shared, y: shared }).indexOf('&') === -1, true);
// lineWidth -1: a long scalar stays on one line.
eq('no folding', U.yamlDump({ a: 'x'.repeat(300) }).split('\n').length, 2);
// sortKeys false: documents are ordered maps (DESIGN §2).
eq('key order preserved', Object.keys(U.yamlLoad(U.yamlDump({ b: 1, a: 2 }))), ['b', 'a']);
// The safe (default) schema: no arbitrary JS types out of a document.
let unsafeThrew = false;
try {
    U.yamlLoad('a: !!js/function "function () {}"\n');
} catch (_e) {
    unsafeThrew = true;
}
eq('default schema rejects !!js/function', unsafeThrew, true);

console.log('\n--- YAML property test: load(dump(x)) deep-equals x ---');
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
        text = U.yamlDump(doc);
        back = U.yamlLoad(text);
    } catch (err) {
        propFails++;
        if (propFails <= 3) {
            console.log(`FAIL seed ${i}: ${err.message}\n  doc  ${JSON.stringify(doc)}`);
        }
        continue;
    }
    // JSON.stringify compares structure, values *and* key order, which is what an
    // ordered-map document model needs.
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

console.log('\n--- row merge: document + grammar ---');
const P = ctx.PVE.meta.TreePanel;
const panel = {
    dc: false,
    tags: ['traefik'],
    access: { read: 1, write: 1, scopes: [] },
    registrations: [
        {
            name: 'traefik',
            authid: 'svc@pve!traefik',
            scopes: [
                {
                    prefix: 'traefik',
                    mode: 'rw',
                    selector: { tag: 'traefik' },
                    grammar: {
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
                    },
                },
            ],
        },
        {
            name: 'netbird',
            authid: 'svc@pve!netbird',
            scopes: [{ prefix: 'netbird', mode: 'ro', selector: { all: true } }],
        },
    ],
};
[
    'entry',
    'addData',
    'addGrammar',
    'schemaKind',
    'applicableScopes',
    'resolvedScopeApplies',
    'accessFor',
    'accessSummary',
    'editableFor',
].forEach((m) => (panel[m] = P[m]));

const scopes = panel.applicableScopes.call(panel);
eq('applicable scopes', scopes.map((s) => s.prefix), ['traefik', 'netbird']);

const root = { key: '', path: '', children: {}, present: true, kind: 'map' };
panel.addData.call(panel, root, storeDoc);
scopes.forEach((s) => (s.grammar ? panel.addGrammar.call(panel, root, s.prefix, s.grammar) : null));

const spec = root.children.traefik.children.spec.children;
eq('grammar adds unset rows', Object.keys(spec).sort(), ['host', 'port', 'scheme']);
eq('present key stays present', spec.host.present, true);
eq('declared key is unset', spec.port.present, false);
eq('display boolean', U.displayValue(1, 'boolean'), 'Yes');
eq('declared default carried', spec.port.defaultValue, 80);
eq('declared enum carried', spec.scheme.enumValues, ['http', 'https']);
// The grammar's description is the tooltip, not the Description column (DESIGN §8).
eq('grammar description is its own field', spec.host.grammarDescription, 'Public host name');
eq('grammar description is not the comment', spec.host.description, undefined);
eq('schema kind integer', panel.schemaKind({ type: 'integer' }), 'number');

// A declared type wins over the type inferred from the stored value.
root.children.traefik.children.spec.children.host.kind = 'number';
panel.addGrammar.call(panel, root, 'traefik', panel.registrations[0].scopes[0].grammar);
eq('grammar type wins', root.children.traefik.children.spec.children.host.kind, 'string');

// Comment key becomes the sibling's Description, never a row of its own.
eq('comment not a row', Object.keys(root.children.netbird.children).sort(), ['groups']);
eq('comment is the description', root.children.netbird.children.groups.description, 'asdf');
eq('array stays one leaf', root.children.netbird.children.groups.kind, 'array');

console.log('\n--- Access: every registration whose scope covers the row ---');
eq('access of a grammar row', panel.accessFor.call(panel, 'traefik.spec.port', scopes), [
    { name: 'traefik', mode: 'rw', selector: 'tag: traefik', prefix: 'traefik' },
]);
eq('access ro marked', panel.accessSummary(panel.accessFor.call(panel, 'netbird.groups', scopes)), 'netbird (ro)');
eq('access of an unclaimed row', panel.accessFor.call(panel, 'mine.key', scopes), []);
// Several principals may cover the same subtree; rw sorts before ro.
const overlapping = scopes.concat([
    {
        prefix: 'traefik',
        mode: 'ro',
        selector: { all: true },
        registration: { name: 'audit', authid: 'svc@pve!audit' },
    },
]);
eq(
    'rw first, then ro',
    panel.accessSummary(panel.accessFor.call(panel, 'traefik.spec.host', overlapping)),
    'traefik, audit (ro)',
);

panel.access = { read: 1, write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] };
eq('scoped write inside', panel.editableFor.call(panel, 'traefik.spec.host'), true);
eq('scoped write outside', panel.editableFor.call(panel, 'netbird.groups'), false);

console.log('\n--- S6: scope-only principal (no VM.Audit, so no tags) ---');
// No `me.tags` (as a caller without VM.Audit gets from GET /meta/guests), but
// GET /meta/access already resolved this caller's own tag-selector scope.
const scopedPanel = Object.assign({}, panel);
scopedPanel.tags = [];
scopedPanel.access = { read: 0, write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] };
const scopedScopes = panel.applicableScopes.call(scopedPanel);
// `netbird` is still applicable regardless of tags (its selector is `all: true`);
// `traefik` is a `tag` selector that only resolves via GET /meta/access now.
eq('resolved-scope applicability (no tags visible)', scopedScopes.map((s) => s.prefix).sort(), [
    'netbird',
    'traefik',
]);
eq(
    'access label still comes from the registration',
    panel.accessSummary(panel.accessFor.call(scopedPanel, 'traefik.spec.host', scopedScopes)),
    'traefik',
);

console.log('\n--- grammar findings for the text editor (mirrors ui/src/lint.rs) ---');
const L = ctx.PVE.meta.Lint;
const GRAMMAR = [
    [
        'traefik',
        {
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
    ],
];

eq('a clean document has no findings',
    L.findings({ traefik: { spec: { host: 'a.example', port: 80, scheme: 'https', enabled: true } } },
        GRAMMAR),
    []);

eq('each rule is reported at its own path',
    L.findings({ traefik: { spec: { port: 70000, scheme: 'ftp', enabled: 'yes' } } }, GRAMMAR)
        .map((f) => f.path),
    ['traefik.spec.enabled', 'traefik.spec.port', 'traefik.spec.scheme']);

// DESIGN section 4: the JSON view renders booleans as 1/0; flagging those would put a
// warning on every boolean in the store.
eq('a boolean on the wire as 1 is not a finding',
    L.findings({ traefik: { spec: { enabled: 1 } } }, GRAMMAR), []);
eq('a boolean on the wire as 0 is not a finding',
    L.findings({ traefik: { spec: { enabled: 0 } } }, GRAMMAR), []);
eq('but 2 is', L.findings({ traefik: { spec: { enabled: 2 } } }, GRAMMAR).length, 1);

eq('keys no grammar describes are left alone',
    L.findings({ traefik: { extra: { anything: [1, 2] } }, mine: { x: 1 } }, GRAMMAR), []);
eq('a prefix with nothing under it contributes nothing', L.findings({}, GRAMMAR), []);
eq('a scope with no grammar contributes nothing',
    L.applicable([{ prefix: 'netbird', mode: 'rw' }]), []);

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
const IDX = L.lineIndex(YAML);
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
const BIDX = L.lineIndex(BLOCK);
eq('block scalar: the key itself', BIDX['compose.file'], 2);
eq('block scalar: the sibling after it', BIDX['compose.name'], 6);
eq('block scalar: its body is not keys', BIDX['compose.file.services'], undefined);
eq('block scalar: nor promoted to the parent', BIDX['compose.services'], undefined);

const QIDX = L.lineIndex('---\n# c\nhost__: note\nhost: a.example\n"quoted: key": 1\n');
eq('comment keys are ordinary keys', QIDX['host__'], 3);
eq('markers and comments are skipped', QIDX['host'], 4);
eq('a quoted key is unquoted', QIDX['quoted: key'], 5);

eq('findings are placed on their lines',
    L.placed(L.findings({ traefik: { spec: { port: 70000 } } }, GRAMMAR),
        L.lineIndex('traefik:\n  spec:\n    port: 70000\n')),
    [{ line: 3, message: 'must be at most 65535' }]);
eq('a finding the text does not carry is dropped, not misplaced',
    L.placed(L.findings({ traefik: { spec: { port: 70000 } } }, GRAMMAR),
        L.lineIndex('unrelated: 1\n')),
    []);

const SCHEMAS = L.schemaIndex(GRAMMAR);
eq('hover: type, range and default',
    L.hoverText(SCHEMAS['traefik.spec.port']), 'integer \u00b7 1..65535 \u00b7 default: 80');
eq('hover: type, format and description',
    L.hoverText(SCHEMAS['traefik.spec.host']), 'string (dns-name) \u00b7 Public host name');
eq('hover: an enum', L.hoverText(SCHEMAS['traefik.spec.scheme']),
    'string \u00b7 one of: http, https');
eq('hover: nothing declared, nothing shown', L.hoverText(undefined), null);

console.log('\n--- S3: document keys colliding with Object.prototype members ---');
// `constructor`/`toString`/`hasOwnProperty` are ordinary, unreserved document
// keys (DESIGN §4) that must become ordinary rows, not resolve through the
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

console.log(fails ? `\n${fails} FAILURE(S)` : '\nall passed');
process.exit(fails ? 1 : 0);
