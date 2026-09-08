// Monaco's two remaining jobs (`docs/DESIGN.md` §8): "Edit as text" for the selected
// subtree, with a YAML/JSON toggle that is presentation only, and the diff that confirms
// applying it.
//
//   node meta-ui-text-check.js [host] [vmid]
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
        mode: 'replace',
        data: JSON.stringify({ traefik: { spec: { host: 'ct200.example', port: 8080 } }, notes: 'n' }),
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
        const puts = [];
        page.on('request', (r) => {
            if (r.url().includes('/api2/json/meta/') && r.method() === 'PUT') puts.push(JSON.parse(r.postData()));
        });
        await page.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(800);

        // --- the whole document, with nothing selected -------------------------
        await L.clickButton(page, 'Edit as text');
        await L.sleep(2500);
        let dialog = await L.modal(page);
        check('the dialog names the whole document when nothing is selected',
            !!dialog && dialog.text.includes('Whole document'), dialog && dialog.text.slice(0, 60));
        check('it offers the YAML/JSON toggle and Apply',
            !!dialog && ['YAML', 'JSON', 'Apply', 'Cancel'].every((b) => dialog.buttons.includes(b)),
            dialog && dialog.buttons.join(','));

        let text = await L.readEditor(page);
        check('Monaco holds the document as YAML', typeof text === 'string' && text.includes('host: ct200.example'),
            JSON.stringify((text || '').slice(0, 60)));

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

        await page.keyboard.press('Escape');
        await L.sleep(600);

        // --- one subtree ------------------------------------------------------
        await L.clickRow(page, 'spec');
        await L.clickButton(page, 'Edit as text');
        await L.sleep(2500);
        dialog = await L.modal(page);
        check('a selected map is the subtree the dialog edits',
            !!dialog && dialog.text.includes('traefik.spec'), dialog && dialog.text.slice(0, 60));
        text = await L.readEditor(page);
        check('and Monaco holds only that subtree',
            typeof text === 'string' && text.includes('host:') && !text.includes('notes:'),
            JSON.stringify((text || '').slice(0, 80)));

        // --- edit, confirm through the diff, apply -----------------------------
        await L.editModel(page, 'ct200.example', 'text.example');
        await L.sleep(500);
        await L.clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(1500);

        const diff = await L.modal(page);
        check('applying goes through a diff confirmation first',
            !!diff && diff.buttons.includes('Apply') && diff.buttons.includes('Back'),
            diff && diff.buttons.join(','));
        const diffEditors = await page.evaluate(() =>
            (window.monaco ? window.monaco.editor.getDiffEditors().length : 0));
        check('the confirmation is a real Monaco diff', diffEditors > 0, String(diffEditors));

        await L.clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2500);

        const stored = await L.doc(host, t, vmid);
        check('the apply wrote the subtree as text', stored.data.traefik.spec.host === 'text.example',
            JSON.stringify(stored.data.traefik.spec));
        check('the write was a text replace on the subtree view',
            puts.some((b) => b.view === 'traefik.spec' && b.mode === 'replace'
                && typeof b.text === 'string' && b.digest),
            JSON.stringify(puts[puts.length - 1]));
        check('the dialog closed and the tree caught up',
            (await L.rows(page)).some((r) => r.key === 'host' && r.value === 'text.example'));

        check('no page errors', errors.length === 0, errors.join(' | '));
        await page.close();
    } finally {
        await browser.close();
    }

    console.log(failures === 0 ? '\nALL OK' : `\n${failures} FAILED`);
    process.exit(failures === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });
