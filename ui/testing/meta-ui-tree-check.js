// The tree renders the document: rows, nesting, comment keys as descriptions, arrays as
// one leaf, and the toolbar's per-row enablement.
//
//   node meta-ui-tree-check.js [host] [vmid]
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium', headless: 'new',
        args: ['--no-sandbox', '--ignore-certificate-errors'],
    });
    let failures = 0;
    const check = (name, ok, detail) => {
        console.log((ok ? 'ok   ' : 'FAIL ') + name + (detail === undefined ? '' : '  ' + detail));
        if (!ok) failures++;
    };

    // Seed the document so the run is deterministic whatever else touched the lab.
    const seed = {
        __: 'metadata for the lab container',
        traefik: { spec: { host: 'ct200.example', port: 8080 }, spec__: 'router definition' },
        netbird: { groups: ['lan', 'dmz'], groups__: 'peer groups this guest joins' },
        notes: 'a plain string value',
    };
    await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', data: JSON.stringify(seed) });

    try {
        const page = await L.open(browser, host, t, `vmid=${vmid}&type=lxc&node=pvemeta-node1`);
        const errors = [];
        page.on('pageerror', (e) => errors.push(String(e)));
        await page.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(800);

        const rows = await L.rows(page);
        const keys = rows.map((r) => r.key);
        console.log(JSON.stringify(rows, null, 1));

        check('no page errors', errors.length === 0, errors.join(' | '));
        // `data` is unordered on the wire; the tree sorts alphabetically.
        check('top level sorted alphabetically',
            keys.indexOf('netbird') < keys.indexOf('notes') && keys.indexOf('notes') < keys.indexOf('traefik'),
            keys.join(','));
        check('nested keys are shown', keys.includes('spec') && keys.includes('host') && keys.includes('port'));
        check('comment keys are not rows', !keys.some((k) => k.endsWith('__')), keys.join(','));

        const groups = rows.find((r) => r.key === 'groups');
        check('an array is one leaf with a JSON value', !!groups && groups.value === '["lan","dmz"]',
            groups && groups.value);
        check('a comment key is the row description',
            !!groups && groups.description === 'peer groups this guest joins', groups && groups.description);
        const spec = rows.find((r) => r.key === 'spec');
        check('a sibling note documents a map row too',
            !!spec && spec.description === 'router definition', spec && spec.description);

        const host_ = rows.find((r) => r.key === 'host');
        check('a scalar shows its value', !!host_ && host_.value === 'ct200.example', host_ && host_.value);
        const port = rows.find((r) => r.key === 'port');
        check('an integer shows unquoted', !!port && port.value === '8080', port && port.value);
        const notes = rows.find((r) => r.key === 'notes');
        check('a string shows unquoted', !!notes && notes.value === 'a plain string value', notes && notes.value);

        // The lab registration ("scoped") covers `netbird` with an `all` selector, so it
        // owns that subtree on every guest; `notes` is covered by nothing.
        const netbird = rows.find((r) => r.key === 'netbird');
        check('the owner column names the covering registration and its selector',
            !!netbird && netbird.owner === 'scoped (all)', netbird && netbird.owner);
        check('an owned subtree passes the owner down to its children',
            !!groups && groups.owner === 'scoped (all)', groups && groups.owner);
        check('a key no scope covers has no owner',
            !!notes && notes.owner === '', notes && notes.owner);

        // The document's own `__` note rides in the identity line, not as a row.
        const header = await page.evaluate(() => document.body.innerText.slice(0, 400));
        check('the document note is in the header',
            header.includes('metadata for the lab container'), header.split('\n')[1]);
        check('the guest name is in the header', /test-ct-\d+/.test(header), header.split('\n')[0]);

        let toolbar = await L.toolbarButtons(page);
        const labels = toolbar.map((b) => b.label);
        check('the toolbar is Add / Edit / Remove / Edit as text / Refresh',
            ['Add', 'Edit', 'Remove', 'Edit as text'].every((l) => labels.includes(l)), labels.join(','));
        check('Edit and Remove start disabled (nothing selected)',
            toolbar.find((b) => b.label === 'Edit').disabled && toolbar.find((b) => b.label === 'Remove').disabled);

        await L.clickRow(page, 'host');
        toolbar = await L.toolbarButtons(page);
        check('selecting a writable row enables Edit and Remove',
            !toolbar.find((b) => b.label === 'Edit').disabled && !toolbar.find((b) => b.label === 'Remove').disabled);

        await page.close();

        // --- the datacenter document -----------------------------------------
        const dc = await L.open(browser, host, t, '');
        dc.on('pageerror', (e) => errors.push(String(e)));
        await dc.goto(L.url(host, 'dc=1'), { waitUntil: 'networkidle2' });
        await L.sleep(4000);
        const dcRows = await L.rows(dc);
        const dcBody = await dc.evaluate(() => document.body.innerText.slice(0, 200));
        check('the datacenter document renders as a tree too', dcRows.length > 0,
            dcRows.map((r) => r.key).join(','));
        check('it is titled Datacenter', dcBody.includes('Datacenter'), dcBody.split('\n')[4]);
        // §3: scopes apply to guest documents only.
        check('and carries no owners, because scopes do not apply to it',
            dcRows.every((r) => r.owner === ''),
            dcRows.map((r) => r.owner).join('|'));
        await dc.close();
    } finally {
        await browser.close();
    }

    console.log(failures === 0 ? '\nALL OK' : `\n${failures} FAILED`);
    process.exit(failures === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });
