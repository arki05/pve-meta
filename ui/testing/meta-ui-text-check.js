// Monaco's three jobs (`docs/DESIGN.md` section 8): "Edit selection as text" for the
// selected subtree, the Text half of the body's `Tree | Text` toggle for the whole
// document, and the diff that confirms applying either one.
//
//   node meta-ui-text-check.js [host] [vmid]
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const seed = () => L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
        mode: 'replace',
        data: JSON.stringify({ traefik: { spec: { host: 'ct200.example', port: 8080 } }, notes: 'n' }),
    });
    await seed();

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
        const puts = [];
        page.on('request', (r) => {
            if (r.url().includes('/api2/json/meta/') && r.method() === 'PUT') puts.push(JSON.parse(r.postData()));
        });
        await page.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(800);

        // --- "Edit selection as text" needs a selection -------------------------
        let toolbar = await L.toolbarButtons(page);
        check('the action is disabled with nothing selected',
            toolbar.find((b) => b.label === 'Edit selection as text').disabled);

        await L.clickRow(page, 'spec');
        await L.clickButton(page, 'Edit selection as text');
        await L.sleep(2500);
        let dialog = await L.modal(page);
        check('the dialog names the selected subtree',
            !!dialog && dialog.text.includes('traefik.spec'), dialog && dialog.text.slice(0, 60));
        check('it offers the YAML/JSON toggle and Apply',
            !!dialog && ['YAML', 'JSON', 'Apply', 'Cancel'].every((b) => dialog.buttons.includes(b)),
            dialog && dialog.buttons.join(','));

        let text = await L.readEditor(page);
        check('Monaco holds only that subtree',
            typeof text === 'string' && text.includes('host: ct200.example') && !text.includes('notes:'),
            JSON.stringify((text || '').slice(0, 80)));

        await L.clickButton(page, 'JSON', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(700);
        text = await L.readEditor(page);
        check('the toggle re-renders the same subtree as JSON',
            typeof text === 'string' && text.trim().startsWith('{') && text.includes('"host"'),
            JSON.stringify((text || '').slice(0, 60)));

        await L.clickButton(page, 'YAML', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(700);
        text = await L.readEditor(page);
        check('and back to YAML', typeof text === 'string' && text.includes('host: ct200.example'));

        // --- edit, confirm through the diff, apply -----------------------------
        await L.editModel(page, 'ct200.example', 'text.example');
        await L.sleep(500);
        await L.clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(1500);

        let diff = await L.modal(page);
        check('applying goes through a diff confirmation first',
            !!diff && diff.buttons.includes('Apply') && diff.buttons.includes('Back'),
            diff && diff.buttons.join(','));
        let diffEditors = await page.evaluate(() =>
            (window.monaco ? window.monaco.editor.getDiffEditors().length : 0));
        check('the confirmation is a real Monaco diff', diffEditors > 0, String(diffEditors));

        await L.clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2500);

        let stored = await L.doc(host, t, vmid);
        check('the apply wrote the subtree as text', stored.data.traefik.spec.host === 'text.example',
            JSON.stringify(stored.data.traefik.spec));
        check('the write was a text replace on the subtree view',
            puts.some((b) => b.view === 'traefik.spec' && b.mode === 'replace'
                && typeof b.text === 'string' && b.digest),
            JSON.stringify(puts[puts.length - 1]));
        check('the dialog closed and the tree caught up',
            (await L.rows(page)).some((r) => r.key === 'host' && r.value === 'text.example'));

        // --- the Tree | Text toggle swaps the body ------------------------------
        await L.clickButton(page, 'Text');
        await L.sleep(3000);
        toolbar = await L.toolbarButtons(page);
        let labels = toolbar.map((b) => b.label);
        check('the Text body replaces the tree actions with Apply / Discard',
            ['YAML', 'JSON', 'Apply', 'Discard'].every((l) => labels.includes(l))
            && !labels.includes('Add') && !labels.includes('Remove'),
            labels.join(','));
        check('the toggle shows Text as the active half',
            toolbar.find((b) => b.label === 'Text').pressed);
        check('the grid is gone', (await L.rows(page)).length === 0);

        text = await L.readEditor(page);
        check('Monaco holds the whole document as YAML',
            typeof text === 'string' && text.includes('host: text.example') && text.includes('notes:'),
            JSON.stringify((text || '').slice(0, 80)));
        check('Apply and Discard start disabled — nothing has changed yet',
            toolbar.find((b) => b.label === 'Apply').disabled
            && toolbar.find((b) => b.label === 'Discard').disabled);

        // --- Discard puts the loaded text back ---------------------------------
        await L.editModel(page, 'notes: n', 'notes: scribbled');
        await L.sleep(700);
        toolbar = await L.toolbarButtons(page);
        check('typing enables Apply and Discard',
            !toolbar.find((b) => b.label === 'Apply').disabled
            && !toolbar.find((b) => b.label === 'Discard').disabled);
        await L.clickButton(page, 'Discard');
        await L.sleep(900);
        text = await L.readEditor(page);
        check('Discard restores the loaded document', !/scribbled/.test(text || ''),
            JSON.stringify((text || '').slice(0, 80)));

        // --- leaving a dirty Text body asks first -------------------------------
        await L.editModel(page, 'notes: n', 'notes: unsaved');
        await L.sleep(700);
        await L.clickButton(page, 'Tree');
        await L.sleep(900);
        const ask = await L.modal(page);
        check('switching back while dirty asks first',
            !!ask && /discard/i.test(ask.text), ask && ask.text.slice(0, 80));
        await L.clickButton(page, 'No', '.pwt-dialog:not(.pwt-dropdown)')
            .catch(() => page.keyboard.press('Escape'));
        await L.sleep(900);
        check('declining keeps the Text body and what was typed',
            /unsaved/.test((await L.readEditor(page)) || ''));

        // --- and applying the whole document is a root replace -------------------
        const before = puts.length;
        await L.clickButton(page, 'Apply');
        await L.sleep(1500);
        diff = await L.modal(page);
        check('the whole-document apply is diff-confirmed too',
            !!diff && diff.buttons.includes('Apply') && diff.buttons.includes('Back'),
            diff && diff.buttons.join(','));
        diffEditors = await page.evaluate(() =>
            (window.monaco ? window.monaco.editor.getDiffEditors().length : 0));
        check('and it is a real Monaco diff', diffEditors > 0, String(diffEditors));
        await L.clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(3000);

        stored = await L.doc(host, t, vmid);
        check('the whole document was replaced', stored.data.notes === 'unsaved',
            JSON.stringify(stored.data));
        check('as a text replace on the root view with the digest',
            puts.slice(before).some((b) => (b.view === '' || b.view === undefined)
                && b.mode === 'replace' && typeof b.text === 'string' && b.digest),
            JSON.stringify(puts[puts.length - 1]));

        // --- back to the tree, cleanly ------------------------------------------
        await L.clickButton(page, 'Tree');
        await L.sleep(1500);
        check('the tree comes back without asking once the buffer is clean',
            (await L.rows(page)).some((r) => r.key === 'notes' && r.value === 'unsaved'),
            JSON.stringify((await L.rows(page)).map((r) => r.key)));

        check('no page errors', errors.length === 0, errors.join(' | '));
        await page.close();
    } finally {
        await browser.close();
    }

    console.log(failures === 0 ? '\nALL OK' : `\n${failures} FAILED`);
    process.exit(failures === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });
