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
    'PVE.meta.DeclareKeyWindow',
    'PVE.meta.EditValueWindow',
    'PVE.meta.TextWindow',
    'PVE.meta.TreePanel',
    'PVE.meta.DocumentWindow',
    'PVE.meta.NewRegistryWindow',
    'PVE.meta.RegistryGrid',
    'PVE.meta.DatacenterPanel',
]);

console.log('\n--- covers / paths (shared fixture, mirrored in Rust) ---');
// `covers` is mirrored in crates/pve-meta-core/src/scopes.rs on purpose: the server
// enforces the rule, this editor predicts it, and an editor that predicts it
// differently shows rows a write then rejects. Both suites read the same table, so a
// case added on one side cannot be missing on the other. Add cases to the file.
const coversCases = JSON.parse(
    fs.readFileSync(path.join(__dirname, '..', '..', 'testdata', 'covers-cases.json'), 'utf8'),
).cases;
eq('the shared covers fixture is present', coversCases.length >= 15, true);
coversCases.forEach((c) => {
    eq(`covers(${JSON.stringify(c.prefix)}, ${JSON.stringify(c.path)}) -- ${c.why}`,
        U.covers(c.prefix, c.path), c.covered);
});
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
    dc: false,
    tags: ['traefik'],
    access: { read: 1, write: 1, scopes: [] },
    // Two lists now, two rules (DESIGN section 3): prefixes decide shape, grants
    // decide access.
    prefixes: [
        { prefix: 'traefik', selector: { tag: 'traefik' }, schema: TRAEFIK_SCHEMA },
        { prefix: 'netbird', selector: { all: true } },
    ],
    grants: [
        {
            name: 'traefik',
            authid: 'svc@pve!traefik',
            grants: [{ prefix: 'traefik', mode: 'rw', selector: { tag: 'traefik' } }],
        },
        {
            name: 'netbird',
            authid: 'svc@pve!netbird',
            grants: [{ prefix: 'netbird', mode: 'ro', selector: { all: true } }],
        },
    ],
};
[
    'entry',
    'addData',
    'addGrammar',
    'schemaKind',
    'applicablePrefixes',
    'applicableGrants',
    'accessFor',
    'accessSummary',
    'editableFor',
].forEach((m) => (panel[m] = P[m]));

const prefixes = panel.applicablePrefixes.call(panel);
eq('applicable prefixes', prefixes.map((n) => n.prefix), ['traefik', 'netbird']);
const scopes = panel.applicableGrants.call(panel);
eq('applicable grants', scopes.map((s) => s.prefix), ['traefik', 'netbird']);

const root = { key: '', path: '', children: {}, present: true, kind: 'map' };
panel.addData.call(panel, root, storeDoc);
prefixes.forEach((ns) =>
    // The 5-argument form buildTree actually uses -- the 3-argument one silently
    // disables pruning, so a test using it is not testing what the panel does.
    ns.schema ? panel.addGrammar.call(panel, root, ns.prefix, ns.schema, prefixes, ns) : null,
);

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
panel.addGrammar.call(panel, root, 'traefik', TRAEFIK_SCHEMA, prefixes, prefixes[0]);
eq('grammar type wins', root.children.traefik.children.spec.children.host.kind, 'string');

// Comment key becomes the sibling's Description, never a row of its own.
eq('comment not a row', Object.keys(root.children.netbird.children).sort(), ['groups']);
eq('comment is the description', root.children.netbird.children.groups.description, 'asdf');
eq('array stays one leaf', root.children.netbird.children.groups.kind, 'array');

console.log('\n--- Access: every grant whose prefix covers the row ---');
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
        grant: { name: 'audit', authid: 'svc@pve!audit' },
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

console.log('\n--- S6: a guest that does not carry the tag ---');
// A tag selector resolves against this guest's tags and nothing else. Holding a
// `traefik` rw scope of our own must not drag another principal's tag-selected
// grant onto a guest that is not tagged `traefik` -- the Access column would
// then name a writer who cannot in fact write here.
const untagged = Object.assign({}, panel);
untagged.tags = [];
untagged.access = { read: 1, write: 1, scopes: [{ prefix: 'traefik', mode: 'rw' }] };
const untaggedScopes = panel.applicableGrants.call(untagged);
eq('a tag grant needs the tag', untaggedScopes.map((s) => s.prefix), ['netbird']);
eq(
    'no Access row for the prefix whose selector missed',
    panel.accessFor.call(untagged, 'traefik.spec.host', untaggedScopes),
    [],
);
// Same rule on the shape side: no tag, no declared rows from that prefix.
eq(
    'prefix applicability follows the same tags',
    panel.applicablePrefixes.call(untagged).map((n) => n.prefix),
    ['netbird'],
);

console.log('\n--- schema findings for the text editor ---');
const L = ctx.PVE.meta.Lint;
// Prefix objects, the same shape GET /meta/prefixes returns.
const GRAMMAR = L.applicable([
    {
        prefix: 'traefik',
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
eq('a prefix with no schema contributes nothing',
    L.applicable([{ prefix: 'netbird' }]), []);

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

console.log('\n--- nesting: most-specific wins, schemas never merge ---');
// `homelab` and `homelab.docker` are both prefixes. The child governs its whole
// subtree; the parent's own `properties.docker` is shadowed, not combined
// (DESIGN section 3.1). Before revision 6 both walked and their findings unioned.
const NESTED = L.applicable([
    {
        prefix: 'homelab',
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
        schema: { type: 'object', properties: { compose: { type: 'string' } } },
    },
]);
eq('applicable sorts longest prefix first', NESTED.map((n) => n.prefix), ['homelab.docker', 'homelab']);
// One implementation of the rule, in Utils, shared by the row builder, the linter
// and the hover index.
eq('governing picks the child for the child subtree',
    U.governing('homelab.docker.compose', NESTED).prefix, 'homelab.docker');
eq('governing picks the parent elsewhere', U.governing('homelab.notes', NESTED).prefix, 'homelab');
eq('governing picks the child for the boundary itself',
    U.governing('homelab.docker', NESTED).prefix, 'homelab.docker');
eq('governing returns null off-prefix', U.governing('unrelated.x', NESTED), null);

// The parent declares `docker: string` and the document has a map there. That is a
// finding only if the parent is allowed to reach into the child -- it is not.
const NESTED_DOC = { homelab: { notes: 'ok', docker: { compose: 'services: {}' } } };
eq('the parent does not lint the child subtree', L.findings(NESTED_DOC, NESTED), []);

// The child does lint its own subtree.
eq('the child lints its own subtree',
    L.findings({ homelab: { docker: { compose: 42 } } }, NESTED).map((f) => f.path + ': ' + f.message),
    ['homelab.docker.compose: expected string']);

// And the parent still lints what it does own.
eq('the parent lints its own keys',
    L.findings({ homelab: { notes: 7 } }, NESTED).map((f) => f.path),
    ['homelab.notes']);

// Hovers resolve to the governing prefix rather than to whichever was collected
// last -- which used to depend on iteration order.
const NESTED_IDX = L.schemaIndex(NESTED);
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
            // prefix governs that subtree entirely (DESIGN section 3.1).
            schema: {
                type: 'object',
                properties: {
                    notes: { type: 'string' },
                    docker: { type: 'string', description: 'the parent should not win here' },
                },
            },
        },
    ];
    const nsList = nsPanel.applicablePrefixes.call(nsPanel);
    const r = { key: '', path: '', children: {}, present: true, kind: 'map' };
    nsPanel.addData.call(nsPanel, r, { homelab: { docker: { compose: 'x' } } });
    // Exactly what buildTree does: no call-site guard any more, the walk prunes itself.
    nsList.forEach((ns) =>
        ns.schema ? nsPanel.addGrammar.call(nsPanel, r, ns.prefix, ns.schema, nsList, ns) : null,
    );
    const dockerRow = r.children.homelab.children.docker;
    eq('the child governs the boundary row kind', dockerRow.kind, 'map');
    eq('the parent does not describe the child row', dockerRow.grammarDescription, undefined);
    eq('the child declares its own keys', Object.keys(dockerRow.children).sort(), ['compose']);
    eq('the parent still declares its own', r.children.homelab.children.notes.kind, 'string');
}

console.log('\n--- the client model must agree with the server model ---');
// js-yaml's DEFAULT_SCHEMA resolves implicit timestamps; the store does not. Under the
// default, a *presentation-only* YAML/JSON toggle or the Format button rewrote
// `2020-01-01` to "2020-01-01T00:00:00.000Z" and Apply wrote that back.
eq('a bare date stays the string the server stores',
    U.yamlLoad('date: 2020-01-01\n'), { date: '2020-01-01' });
eq('... and survives a dump/load round trip unchanged',
    U.yamlLoad(U.yamlDump({ date: '2020-01-01' })), { date: '2020-01-01' });
eq('a quoted numeric string is still a string', U.yamlLoad('v: "1"\n'), { v: '1' });
eq('an unquoted integer is still a number', U.yamlLoad('v: 1\n'), { v: 1 });
eq('booleans still parse', U.yamlLoad('v: true\n'), { v: true });
eq('an empty document is the empty map, not null', U.yamlLoad(''), {});

console.log('\n--- governing uses containment, not the grant predicate ---');
// `covers` aliases the sibling comment key `p__` -- that is a GRANT rule. Using it to
// pick a governing prefix made `a` govern the whole `a__` prefix, where Rust's
// registry::governing (plain containment) says `a__`.
eq('covers aliases the comment key (grant rule)', U.covers('a', 'a__'), true);
eq('containsPath does not (prefix rule)', U.containsPath('a', 'a__'), false);
eq('containsPath: the prefix itself', U.containsPath('a', 'a'), true);
eq('containsPath: a child', U.containsPath('a', 'a.b'), true);
eq('containsPath: not a name prefix', U.containsPath('a', 'ab'), false);
{
    const two = U.bySpecificity([{ prefix: 'a' }, { prefix: 'a__' }]);
    eq('a comment-key prefix governs itself, not its subject',
        U.governing('a__', two).prefix, 'a__');
}

console.log('\n--- nesting: a schema-less prefix still shadows ---');
{
    // A prefix may declare a selector and no schema (the lab's `netbird` does).
    // It still governs its subtree -- so a parent's schema must not reach into it.
    const all = [
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
    ];
    const doc = { homelab: { notes: 'ok', docker: { compose: 'x' } } };
    eq('a schema-less child still shadows its parent',
        L.findings(doc, L.applicable(all), all).map((f) => f.path), []);
    eq('the parent still lints what it owns',
        L.findings({ homelab: { notes: 7 } }, L.applicable(all), all).map((f) => f.path),
        ['homelab.notes']);
    eq('and the hover index does not cross the boundary either',
        L.schemaIndex(L.applicable(all), all)['homelab.docker'], undefined);
}

console.log('\n--- round trip: a view toggle must not invent changes ---');
// serde_yaml and js-yaml lay the same document out differently, so re-dumping on the
// way back from JSON made a *presentation* toggle report unsaved changes.
const SERVER_YAML = [
    'traefik:',
    '  spec:',
    '    host: a.example',
    '    port: 80',
    '  routers:',
    '  - rule: Host(`a`)',
    '',
].join('\n');
const parsed = U.yamlLoad(SERVER_YAML);
eq('a js-yaml redump differs from the server text (the bug\'s premise)',
    U.yamlDump(parsed) !== SERVER_YAML, true);
eq('sameDocument sees through the layout difference',
    U.sameDocument(parsed, SERVER_YAML), true);
eq('sameDocument says no when a value really changed',
    U.sameDocument({ traefik: { spec: { host: 'b.example' } } }, SERVER_YAML), false);
eq('sameDocument says no when only the key order changed (order is data)',
    U.sameDocument({ b: 1, a: 2 }, 'a: 2\nb: 1\n'), false);
eq('sameDocument on unparseable text is not a match',
    U.sameDocument({}, 'a:\n  - [\n'), false);

console.log('\n--- many documents in one panel ---');
const D = ctx.PVE.meta.DeclareKeyWindow;
// An id is an address: the path it is served at, for every kind of document.
eq('a guest id', P.urlFor.call(P, '201'), '/meta/guests/201');
eq('the datacenter id', P.urlFor.call(P, 'datacenter'), '/meta/datacenter');
eq('a prefix id', P.urlFor.call(P, 'prefixes/homelab.docker'), '/meta/prefixes/homelab.docker');
eq('a grant id', P.urlFor.call(P, 'grants/scoped'), '/meta/grants/scoped');
eq('kind of a guest', P.docKind.call(P, '201'), 'guest');
eq('kind of the datacenter', P.docKind.call(P, 'datacenter'), 'datacenter');
eq('kind of a prefix', P.docKind.call(P, 'prefixes/traefik'), 'prefix');
eq('kind of a grant', P.docKind.call(P, 'grants/scoped'), 'grant');
eq('the title is the file name', P.docTitle.call(P, 'prefixes/homelab.docker'), 'homelab.docker');

// Per-document digests. One shared field would have sent a prefix's digest with a
// write to the datacenter, which is a 409 at best and the wrong document at worst.
{
    const panelM = Object.assign({}, panel, {
        docState: { datacenter: { digest: 'aaa', data: { a: 1 } }, 'prefixes/x': { digest: 'bbb', data: {} } },
    });
    ['digestOf', 'dataOf', 'docOf'].forEach((m) => (panelM[m] = P[m]));
    panelM.docId = 'datacenter';
    eq('each document keeps its own digest', panelM.digestOf('prefixes/x'), 'bbb');
    eq('and its own data', panelM.dataOf('datacenter'), { a: 1 });
    eq('an unknown document has no digest', panelM.digestOf('grants/nope'), '');
    eq('a row names its document', panelM.docOf({ data: { docId: 'prefixes/x' } }), 'prefixes/x');
    eq('no row means the default one', panelM.docOf(null), 'datacenter');
}

// What describes each kind of document. The datacenter document gets nothing:
// prefixes reach guest documents only (DESIGN §3.3).
{
    const META = { type: 'object', properties: { selector: { type: 'object' } } };
    const panelG = Object.assign({}, panel, { dc: true, schemas: { prefix: META, grant: {} } });
    ['grammarFor', 'docKind', 'applicablePrefixes'].forEach((m) => (panelG[m] = P[m]));
    eq('a prefix document is described by the meta-schema',
        panelG.grammarFor('prefixes/x').map((g) => g.prefix), ['']);
    eq('... which is the schema served for its kind',
        panelG.grammarFor('prefixes/x')[0].schema, META);
    eq('the datacenter document is described by nothing', panelG.grammarFor('datacenter'), []);
}

// A root-prefix schema governs the whole document. It must not be pruned away: the
// prune asks `governing`, which answers about prefixes, and '' is not one.
{
    const META = {
        type: 'object',
        properties: {
            selector: { type: 'object', properties: { tag: { type: 'string' } } },
            description: { type: 'string' },
        },
    };
    const rooted = [{ prefix: '', schema: META }];
    eq(
        'the meta-schema lints the document it is rooted at',
        L.findings({ description: 5, selector: { tag: 7 } }, rooted, rooted).map((f) => f.path + ': ' + f.message),
        ['description: expected string', 'selector.tag: expected string'],
    );
    eq(
        'and indexes it for hovers',
        Object.keys(L.schemaIndex(rooted, rooted)).sort(),
        ['', 'description', 'selector', 'selector.tag'],
    );
    // The same rows the tree would show, including a declared-but-unset one.
    const panelR = Object.assign({}, panel, {
        dc: true,
        docState: { 'prefixes/x': { digest: 'd', data: { selector: { tag: 'traefik' } } } },
        schemas: { prefix: META },
    });
    ['grammarFor', 'docKind', 'documentEntries', 'addData', 'addGrammar', 'entry', 'schemaKind', 'dataOf', 'plannedData', 'applicablePrefixes'].forEach(
        (m) => (panelR[m] = P[m]),
    );
    panelR.pending = [];
    panelR.docId = 'prefixes/x';
    const entries = panelR.documentEntries();
    eq('a prefix document shows its declared keys', Object.keys(entries.children).sort(), ['description', 'selector']);
    eq('what it holds is present', entries.children.selector.children.tag.present, true);
    eq('what it does not hold is a declared-but-unset row', entries.children.description.present, false);
}

console.log('\n--- staged edits: the change that had no legal single step ---');
{
    // The case that forced this: a prefix definition's selector is exactly one of
    // `all` or `tag`, so `{all: true}` -> `{tag: web}` has NO valid intermediate.
    // Dropping `all` first is refused by the server; adding `tag` first is refused;
    // the row editor could only ever do one at a time. Reproduced on the lab.
    const stored = { description: 'Home', selector: { all: true } };
    const pending = [
        { path: 'selector.all', op: 'delete' },
        { path: 'selector.tag', op: 'set', value: 'web' },
    ];
    eq('both edits land in one planned document', U.applyPending(stored, pending), {
        description: 'Home',
        selector: { tag: 'web' },
    });
    // ... and go out as ONE write, at the narrowest view covering both.
    eq('written as one view', U.writeView(pending), 'selector');
    eq('the stored document is untouched until then', stored, {
        description: 'Home',
        selector: { all: true },
    });

    // A single row edit is still exactly the one-key write it always was.
    eq('one set writes that key', U.writeView([{ path: 'a.b.c', op: 'set', value: 1 }]), 'a.b.c');
    // A delete cannot be expressed by replacing the thing being deleted, so the
    // write moves one level up and replaces the parent without the key.
    eq('one delete writes its parent', U.writeView([{ path: 'a.b.c', op: 'delete' }]), 'a.b');
    eq('a top-level delete writes the document', U.writeView([{ path: 'a', op: 'delete' }]), '');
    eq('unrelated subtrees write the document', U.writeView([
        { path: 'traefik.spec.host', op: 'set', value: 'x' },
        { path: 'netbird.groups', op: 'set', value: [] },
    ]), '');
    eq('nothing staged, nothing to write', U.writeView([]), null);

    // Deletes and sets applied in the order they were made.
    eq('order is what was done', U.applyPending({ a: 1 }, [
        { path: 'a', op: 'delete' },
        { path: 'a', op: 'set', value: 2 },
    ]), { a: 2 });
    eq('a set then a delete leaves nothing', U.applyPending({}, [
        { path: 'x.y', op: 'set', value: 1 },
        { path: 'x.y', op: 'delete' },
    ]), { x: {} });
    // Intermediate maps are created for a new nested key.
    eq('a new nested key builds its parents', U.applyPending({}, [
        { path: 'schema.properties.port.type', op: 'set', value: 'integer' },
    ]), { schema: { properties: { port: { type: 'integer' } } } });

    // Keys are document data and no key is reserved: staging one named `__proto__`
    // must set a key, not the prototype. (`entry()` guards the same way.)
    const planned = U.applyPending({}, [{ path: '__proto__', op: 'set', value: 'oops' }]);
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
    // A column a tree could not show: a schema-less definition is a real thing
    // (the lab's netbird), and looked identical to one with a schema.
    eq('carrying a schema is a column', rows.map((r) => r.schema), ['yes', '', 'yes']);

    // Three origin states, not two. The middle one is where Remove does not remove.
    eq('a package\'s file', G.originText(rows[0]), 'packaged');
    eq('an administrator\'s own', G.originText(rows[1]), 'cluster');
    eq('one written over a package\'s', G.originText(rows[2]), 'cluster (overrides packaged)');

    const grants = G.rowsFrom('grants', [
        {
            name: 'scoped',
            authid: 'svc@pve!t1',
            grants: [
                { prefix: 'traefik', mode: 'rw', selector: { tag: 'traefik' } },
                { prefix: 'netbird', mode: 'ro', selector: { all: true } },
            ],
            origin: 'cluster',
        },
    ]);
    eq('a grant row is addressed the same way', grants[0].id, 'grants/scoped');
    eq(
        'and says what it actually grants',
        grants[0].summary,
        'traefik (rw, tag: traefik), netbird (ro, all guests)',
    );
    // An older API returns neither field; the list must still render.
    eq('a row with no origin is treated as the cluster\'s', G.originText(G.rowsFrom('grants', [{ name: 'x' }])[0]), 'cluster');
}

console.log('\n--- creating a registry file: the least that parses ---');
{
    const N = ctx.PVE.meta.NewRegistryWindow;
    const make = (kind, v) => N.contentFrom.call({ kind: kind }, v);
    eq('a prefix with an "all" selector', make('prefixes', { selector: 'all' }), { selector: { all: true } });
    eq('a prefix with a tag selector', make('prefixes', { selector: 'tag', tag: 'web' }), { selector: { tag: 'web' } });
    eq(
        'a description when there is one',
        make('prefixes', { selector: 'all', description: 'Home' }),
        { description: 'Home', selector: { all: true } },
    );
    // A grant is created granting nothing: it names a principal, and an
    // administrator says what it may touch afterwards.
    eq('a grant starts empty', make('grants', { authid: 'a@pve!t1' }), { authid: 'a@pve!t1', grants: [] });
}

console.log('\n--- reloading must not fold the tree up ---');
{
    // The key that survives a reload. A document row and a group row both live at the
    // empty path, and the two group rows have no document at all, so keying on
    // (docId, path) collided: collapsing `Prefixes` came back expanded on the next
    // reload because `Grants` had won the shared key.
    const key = (n) => (n.data.docId || '') + '\u0000' + n.data.path + '\u0000' + (n.data.key || '');
    const prefixes = { data: { docId: null, path: '', key: 'Prefixes' } };
    const grants = { data: { docId: null, path: '', key: 'Grants' } };
    const dcRoot = { data: { docId: 'datacenter', path: '', key: 'datacenter' } };
    const nsRoot = { data: { docId: 'prefixes/homelab', path: '', key: 'prefixes/homelab' } };
    const same = { data: { docId: 'prefixes/homelab', path: 'selector', key: 'selector' } };
    const other = { data: { docId: 'grants/scoped', path: 'selector', key: 'selector' } };
    const keys = [prefixes, grants, dcRoot, nsRoot, same, other].map(key);
    eq('every row has a key of its own', new Set(keys).size, keys.length);
}

console.log('\n--- the tree marks a row its schema refuses ---');
{
    // The text editor has squiggled these since revision 6; the tree, which is what
    // people open, said nothing. Same rule, same function -- `grammarSplit` is shared
    // by both callers so they cannot answer differently.
    const SCHEMA = {
        type: 'object',
        properties: { port: { type: 'integer', minimum: 1, maximum: 65535 } },
    };
    const panelF = Object.assign({}, panel, {
        dc: false,
        docState: { 201: { digest: 'd', data: { docker: { port: 70000, host: 'ok' } } } },
        prefixes: [{ prefix: 'docker', selector: { all: true }, schema: SCHEMA }],
        tags: [],
    });
    ['grammarSplit', 'grammarFor', 'findingsFor', 'docKind', 'dataOf', 'plannedData', 'applicablePrefixes'].forEach(
        (m) => (panelF[m] = P[m]),
    );
    panelF.pending = [];
    panelF.docId = '201';
    const found = panelF.findingsFor();
    eq('the out-of-range row is marked', found['docker.port'], 'must be at most 65535');
    eq('a row that fits is not', found['docker.host'], undefined);

    // A registry document is linted by its meta-schema through the same call.
    const panelR = Object.assign({}, panel, {
        dc: true,
        docState: { 'prefixes/x': { digest: 'd', data: { description: 5 } } },
        schemas: { prefix: { type: 'object', properties: { description: { type: 'string' } } } },
    });
    ['grammarSplit', 'grammarFor', 'findingsFor', 'docKind', 'dataOf', 'plannedData', 'applicablePrefixes'].forEach(
        (m) => (panelR[m] = P[m]),
    );
    panelR.pending = [];
    panelR.docId = 'prefixes/x';
    eq(
        'a prefix file is marked against the meta-schema',
        panelR.findingsFor().description,
        'expected string',
    );
    // The datacenter document has no schema at all, so it can never be marked.
    panelR.docState.datacenter = { digest: 'd', data: { anything: 5 } };
    panelR.docId = 'datacenter';
    eq('the datacenter document is never marked', panelR.findingsFor(), {});
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
eq('an enum is a list, not a string', D.schemaFrom({ type: 'string', enum: 'always, no ,unless-stopped' }),
    { type: 'string', enum: ['always', 'no', 'unless-stopped'] });
// A range on a string, or a format on a number, would be a declaration nothing reads.
eq('a range belongs to a number', D.schemaFrom({ type: 'string', minimum: '1', maximum: '9' }), { type: 'string' });
eq('a format belongs to a string', D.schemaFrom({ type: 'integer', format: 'ip' }), { type: 'integer' });
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
// A map has no default the editor would ever read: `addGrammar` stops at an object and
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
    const ns = [{ prefix: 'notes', selector: { all: true }, schema: {
        type: 'object',
        properties: { body: { type: 'string', multiline: 1, description: 'Free text' } },
    } }];
    panel.addGrammar.call(panel, root, 'notes', ns[0].schema, ns, ns[0]);
    eq('multiline reaches the row', root.children.notes.children.body.multiline, true);
}

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
