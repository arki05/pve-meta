// Offline smoke test of the parts of pve-meta-tree.js that are pure JS: the
// YAML codec, the value helpers, and the entry-merge that builds the rows.
// A minimal Ext/PVE shim is enough - none of this touches the DOM.
const fs = require('fs');
const vm = require('vm');
const path = require('path');

const ctx = {
    console,
    window: {},
    document: { createElement: () => ({}), head: { appendChild() {} } },
    Promise,
    gettext: (s) => s,
    Ext: {
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
        tree: { Panel: {} },
        grid: { plugin: { CellEditing: {} } },
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

console.log('\n--- YAML round trip ---');
// The exact canonical dump the live store produced for guest 200.
const storeYaml = 'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups__: asdf\n  groups:\n  - lan\n';
const storeDoc = {
    traefik: { spec: { host: 'ct200.example' } },
    netbird: { groups__: 'asdf', groups: ['lan'] },
};
eq('parse store dump', U.yamlParse(storeYaml), storeDoc);
eq('dump == store dump', U.yamlDump(storeDoc), storeYaml);

const rich = {
    s: 'plain',
    quoted: 'has: colon',
    n: 3,
    f: 1.5,
    b: true,
    empty: {},
    emptyList: [],
    list: ['a', 'b'],
    maps: [{ x: 1, y: 'z' }, { x: 2 }],
    nested: { deep: { deeper: 'v' } },
    'numeric-looking': '007',
};
eq('rich round trip', U.yamlParse(U.yamlDump(rich)), rich);

const roundtrips = [
    { a: '' },
    { a: '#hash' },
    { a: 'true' },
    { a: 'a # b' },
    { a: ' lead' },
    { a: 'multi\nline' },
    { a: { b: [{ c: [1, 2] }] } },
];
roundtrips.forEach((v, i) => eq('roundtrip ' + i, U.yamlParse(U.yamlDump(v)), v));

console.log('\n--- YAML refusals (must throw, never silently mangle) ---');
[
    ['anchor', 'a: &x 1\nb: *x\n'],
    ['flow map', 'a: {b: 1}\n'],
    ['null', 'a: null\n'],
    ['tab indent', 'a:\n\tb: 1\n'],
].forEach(([name, text]) => {
    let threw = false;
    try {
        U.yamlParse(text);
    } catch (e) {
        threw = true;
    }
    // anchors are the one case a naive parser accepts as a string; note it either way
    eq('refuses ' + name, threw, true);
});

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
['entry', 'addData', 'addGrammar', 'schemaKind', 'applicableScopes', 'ownerFor', 'editableFor'].forEach(
    (m) => (panel[m] = P[m]),
);

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
eq('grammar description', spec.host.description, 'Public host name');
eq('schema kind integer', panel.schemaKind({ type: 'integer' }), 'number');

// A declared type wins over the type inferred from the stored value.
root.children.traefik.children.spec.children.host.kind = 'number';
panel.addGrammar.call(panel, root, 'traefik', panel.registrations[0].scopes[0].grammar);
eq('grammar type wins', root.children.traefik.children.spec.children.host.kind, 'string');

// Comment key becomes the sibling's description, never a row of its own.
eq('comment not a row', Object.keys(root.children.netbird.children).sort(), ['groups']);
eq('comment is description', root.children.netbird.children.groups.description, 'asdf');
eq('array stays one leaf', root.children.netbird.children.groups.kind, 'array');

eq('owner of grammar row', panel.ownerFor.call(panel, 'traefik.spec.port', scopes), 'traefik (tag: traefik)');
eq('owner ro marked', panel.ownerFor.call(panel, 'netbird.groups', scopes), 'netbird [ro]');
eq('owner of unclaimed', panel.ownerFor.call(panel, 'mine.key', scopes), '');

panel.access = { read: 1, write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] };
eq('scoped write inside', panel.editableFor.call(panel, 'traefik.spec.host'), true);
eq('scoped write outside', panel.editableFor.call(panel, 'netbird.groups'), false);

console.log(fails ? `\n${fails} FAILURE(S)` : '\nall passed');
process.exit(fails ? 1 : 0);
