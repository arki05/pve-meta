// Row editing: Edit a scalar, edit its description, Add a key, Remove a row — each one a
// `PUT ?view=<path>&mode=replace` (or a `DELETE`) with the digest, verified against the
// API afterwards.
//
//   node meta-ui-write-check.js [host] [vmid]
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const seed = {
        traefik: { spec: { host: 'ct200.example', port: 8080 } },
        netbird: { groups: ['lan'] },
        notes: 'a plain string value',
    };
    await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', data: JSON.stringify(seed) });

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
        const requests = [];
        page.on('request', (r) => {
            if (r.url().includes('/api2/json/meta/') && r.method() !== 'GET') {
                requests.push({ method: r.method(), body: r.postData() });
            }
        });
        await page.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(800);

        // --- edit a scalar, and give it a description ------------------------
        await L.clickRow(page, 'host');
        await L.clickButton(page, 'Edit');
        let dialog = await L.modal(page);
        check('the row dialog is an EditWindow titled with the path',
            !!dialog && dialog.text.includes('traefik.spec.host'), dialog && dialog.text.slice(0, 80));
        check('it starts from the current value',
            !!dialog && dialog.fields.some((f) => f.name === 'Value' && f.value === 'ct200.example'),
            JSON.stringify(dialog && dialog.fields));
        check('it offers the row description too',
            !!dialog && dialog.fields.some((f) => f.name === 'Description'));

        await L.setField(page, 'Value', 'edited.example');
        await L.setField(page, 'Description', 'the public host name');
        await L.clickButton(page, 'Update');
        await L.sleep(2500);

        let stored = await L.doc(host, t, vmid);
        check('the value was written', stored.data.traefik.spec.host === 'edited.example',
            stored.data.traefik.spec.host);
        check('the description became the sibling comment key',
            stored.data.traefik.spec.host__ === 'the public host name', stored.data.traefik.spec.host__);

        const puts = requests.filter((r) => r.method === 'PUT').map((r) => JSON.parse(r.body));
        check('the write is a replace on the row path with the digest',
            puts.some((b) => b.view === 'traefik.spec.host' && b.mode === 'replace'
                && b.data === '"edited.example"' && typeof b.digest === 'string' && b.digest.length > 0),
            JSON.stringify(puts[0]));

        let shown = await L.rows(page);
        check('the tree reloaded and shows the new value',
            shown.some((r) => r.key === 'host' && r.value === 'edited.example'));
        check('the note is now the row description',
            shown.some((r) => r.key === 'host' && r.description === 'the public host name'));

        // --- add a key under a map -------------------------------------------
        await L.clickRow(page, 'spec');
        await L.clickButton(page, 'Add');
        dialog = await L.modal(page);
        check('the add dialog asks for key, type and value',
            !!dialog && ['Key', 'Type', 'Value'].every((n) => dialog.fields.some((f) => f.name === n)),
            JSON.stringify(dialog && dialog.fields.map((f) => f.name)));
        await L.setField(page, 'Key', 'scheme');
        await L.setField(page, 'Value', 'https');
        await L.clickButton(page, 'Add', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2500);

        stored = await L.doc(host, t, vmid);
        check('the new key landed under the selected map', stored.data.traefik.spec.scheme === 'https',
            JSON.stringify(stored.data.traefik.spec));

        // --- add a typed key --------------------------------------------------
        await L.clickRow(page, 'spec');
        await L.clickButton(page, 'Add');
        await L.setField(page, 'Key', 'weight');
        await L.pickCombo(page, 'Type', 'integer');
        await L.setField(page, 'Value', '42');
        await L.clickButton(page, 'Add', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2500);
        stored = await L.doc(host, t, vmid);
        check('an integer is written as a JSON number, not a string',
            stored.data.traefik.spec.weight === 42, JSON.stringify(stored.data.traefik.spec.weight));

        // --- remove a row -----------------------------------------------------
        await L.clickRow(page, 'scheme');
        await L.clickButton(page, 'Remove');
        const confirm = await L.modal(page);
        check('Remove confirms first', !!confirm && /remove/i.test(confirm.text), confirm && confirm.text.slice(0, 80));
        await L.clickButton(page, 'Yes', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2500);

        stored = await L.doc(host, t, vmid);
        check('the row is gone', stored.data.traefik.spec.scheme === undefined,
            JSON.stringify(stored.data.traefik.spec));
        const deletes = requests.filter((r) => r.method === 'DELETE').map((r) => JSON.parse(r.body));
        check('the delete named the view and sent the digest',
            deletes.some((b) => b.view === 'traefik.spec.scheme' && b.digest),
            JSON.stringify(deletes));

        check('no page errors', errors.length === 0, errors.join(' | '));
        await page.close();
    } finally {
        await browser.close();
    }

    console.log(failures === 0 ? '\nALL OK' : `\n${failures} FAILED`);
    process.exit(failures === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });
