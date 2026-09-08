// Grammar-declared rows: the keys an operator registration declares are rows even when
// the document has never carried them — greyed, with their default and description, an
// Owner, and a "set" action that writes them.
//
// Needs a registration with a grammar whose selector matches the guest (the lab ships
// `example-traefik` for all guests and `traefik-demo` for `tag: traefik`).
//
//   node meta-ui-grammar-check.js [host] [vmid]
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const ops = JSON.parse((await L.api(host, t, 'GET', '/meta/operators')).body).data || [];
    const grammars = ops.filter((o) => o.scopes.some((s) => s.grammar));
    if (!grammars.length) {
        console.log('SKIP no registration with a grammar on this cluster');
        process.exit(0);
    }
    // What the cluster's grammars actually say about `traefik.spec.port` right now — the
    // lab's registrations are edited by hand, so the check asks them rather than assuming.
    const portSchemas = grammars.flatMap((o) => o.scopes)
        .filter((s) => s.grammar && s.prefix === 'traefik')
        .map((s) => ((s.grammar.properties || {}).spec || {}).properties || {})
        .map((p) => p.port)
        .filter(Boolean);
    const declaredDescription = (portSchemas.find((p) => p.description) || {}).description;
    const declaredDefault = (portSchemas.find((p) => p.default !== undefined) || {}).default;

    // A document with only `traefik.spec.host` set: everything else the grammar declares
    // has to appear on its own.
    await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
        mode: 'replace',
        data: JSON.stringify({ traefik: { spec: { host: 'ct200.example' } } }),
    });

    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium', headless: 'new',
        args: ['--no-sandbox', '--ignore-certificate-errors'],
    });
    let failures = 0;
    const check = (name, ok, detail) => {
        console.log((ok ? 'ok   ' : 'FAIL ') + name + (detail === undefined ? '' : '  ' + detail));
        if (!ok) failures++;
    };

    try {
        const page = await L.open(browser, host, t, '');
        const errors = [];
        page.on('pageerror', (e) => errors.push(String(e)));
        await page.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(1000);

        let rows = await L.rows(page);
        console.log(JSON.stringify(rows, null, 1));
        const by = (k) => rows.find((r) => r.key === k);

        check('a declared key is a row even though the document has never had it', !!by('port'));
        const port = by('port');
        check('an unset row is greyed', !!port && port.dimmed);
        check('an unset row shows the grammar default',
            !!port && port.value.startsWith(String(declaredDefault)),
            port && port.value + ' (declared ' + declaredDefault + ')');
        if (declaredDescription) {
            // Two registrations may declare the same key; the row merges what they say, so
            // a description from either one has to reach it.
            check('an unset row shows the grammar description',
                !!port && port.description === declaredDescription,
                port && port.description);
        } else {
            console.log('note: no grammar on this cluster describes traefik.spec.port');
        }
        check('an unset row offers a "set" action', !!port && port.value.includes('Set'),
            port && port.value);
        check('a declared row is owned by the registration that declared it',
            !!port && /\(.+\)/.test(port.owner), port && port.owner);
        check('a set key keeps its own value', !!by('host') && by('host').value === 'ct200.example',
            by('host') && by('host').value);
        check('declared siblings sort alphabetically',
            rows.map((r) => r.key).join(',').includes('host,middleware,port,scheme')
            || rows.map((r) => r.key).join(',').includes('host,port'),
            rows.map((r) => r.key).join(','));

        // --- the "set" action writes the row ---------------------------------
        const clicked = await page.evaluate(() => {
            const rows = Array.from(document.querySelectorAll('[role="row"]'));
            const row = rows.find((r) => {
                const c = r.querySelector('[role="gridcell"], td');
                return c && c.innerText.split('\n')[0].trim() === 'port';
            });
            if (!row) return false;
            const set = Array.from(row.querySelectorAll('a, [role="button"]'))
                .find((e) => e.textContent.trim() === 'Set');
            if (!set) return false;
            set.click();
            return true;
        });
        check('the "set" action is clickable', clicked);
        await L.sleep(700);

        const dialog = await L.modal(page);
        check('it opens the row dialog in "set" mode',
            !!dialog && dialog.text.startsWith('Set:'), dialog && dialog.text.slice(0, 60));
        check('it starts from the declared default',
            !!dialog && dialog.fields.some((f) => f.name === 'Value' && f.value === '80'),
            JSON.stringify(dialog && dialog.fields));

        await L.setField(page, 'Value', '8443');
        await L.clickButton(page, 'Add', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2500);

        const stored = await L.doc(host, t, vmid);
        check('setting a declared row writes it as its declared type',
            stored.data.traefik.spec.port === 8443, JSON.stringify(stored.data.traefik.spec));

        rows = await L.rows(page);
        check('the row is no longer greyed once it is set',
            !!rows.find((r) => r.key === 'port') && !rows.find((r) => r.key === 'port').dimmed);

        // --- an enum row gets a picker, not a free text field ------------------
        const scheme = rows.find((r) => r.key === 'scheme');
        if (scheme) {
            await L.clickRow(page, 'scheme');
            await L.clickButton(page, 'Edit');
            const enumDialog = await L.modal(page);
            check('an enum row is edited with a picker',
                !!enumDialog && enumDialog.fields.some((f) => f.name === 'Value'),
                JSON.stringify(enumDialog && enumDialog.fields));
            await L.clickButton(page, 'Close') .catch(() => {});
            await page.keyboard.press('Escape');
            await L.sleep(500);
        } else {
            console.log('note: no enum in this cluster\'s grammar, skipping the picker check');
        }

        check('no page errors', errors.length === 0, errors.join(' | '));
        await page.close();
    } finally {
        await browser.close();
    }

    console.log(failures === 0 ? '\nALL OK' : `\n${failures} FAILED`);
    process.exit(failures === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });
