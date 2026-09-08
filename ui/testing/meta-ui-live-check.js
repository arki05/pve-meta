// The live behaviours: the 5 s version poll refreshing the tree, the poll holding still
// while a row is being edited, a 409 reloading with a notice, and a scoped principal
// getting per-row editability.
//
//   node meta-ui-live-check.js [host] [vmid]
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';
const scopedUser = process.argv[4] || 'scoped@pve';
const scopedPass = process.argv[5] || 'pvelabscoped';

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const seed = () => L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
        mode: 'replace',
        data: JSON.stringify({ traefik: { spec: { host: 'ct200.example' } }, netbird: { groups: ['lan'] } }),
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
        await page.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(800);

        // --- the poll picks a change up ---------------------------------------
        await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
            mode: 'merge', data: JSON.stringify({ traefik: { spec: { host: 'changed.outside' } } }),
        });
        await L.sleep(9000);
        let rows = await L.rows(page);
        check('the version poll refreshes the tree',
            rows.some((r) => r.key === 'host' && r.value === 'changed.outside'),
            JSON.stringify(rows.find((r) => r.key === 'host')));

        // --- the poll holds still while a row is being edited -------------------
        await L.clickRow(page, 'host');
        await L.clickButton(page, 'Edit');
        await L.setField(page, 'Value', 'typed.while.polling');
        await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
            mode: 'merge', data: JSON.stringify({ notes: 'added from outside' }),
        });
        await L.sleep(9000);
        let dialog = await L.modal(page);
        check('the dialog is still open with what was typed',
            !!dialog && dialog.fields.some((f) => f.name === 'Value' && f.value === 'typed.while.polling'),
            JSON.stringify(dialog && dialog.fields));

        // --- and the write from a dialog opened before the change 409s ----------
        await L.clickButton(page, 'Update');
        await L.sleep(3000);
        let body = await page.evaluate(() => document.body.innerText);
        check('a stale write is refused and the page says so',
            /changed on the server/i.test(body), body.split('\n').slice(0, 8).join(' | '));
        const stillTyped = await L.doc(host, t, vmid);
        check('and the stale value was not written',
            stillTyped.data.traefik.spec.host !== 'typed.while.polling',
            stillTyped.data.traefik.spec.host);
        check('and the tree reloaded to the server state',
            (await L.rows(page)).some((r) => r.key === 'notes'),
            (await L.rows(page)).map((r) => r.key).join(','));

        await page.keyboard.press('Escape');
        await L.sleep(500);
        await page.close();

        // --- a scoped principal --------------------------------------------------
        let scoped;
        try {
            scoped = await L.ticket(host, scopedUser, scopedPass);
        } catch (e) {
            console.log('note: no ' + scopedUser + ' on this cluster, skipping the scoped checks');
        }
        if (scoped) {
            await seed();
            const grants = JSON.parse((await L.api(host, scoped, 'GET', `/meta/access?vmid=${vmid}`)).body).data;
            console.log('note: grants for ' + scopedUser + ': ' + JSON.stringify(grants));

            const page2 = await L.open(browser, host, scoped, '');
            page2.on('pageerror', (e) => errors.push(String(e)));
            await page2.goto(L.url(host, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
            await L.waitForTree(page2);
            await L.sleep(800);

            const rows2 = await L.rows(page2);
            check('a scoped principal sees the rows it may read', rows2.length > 0,
                rows2.map((r) => r.key).join(','));

            // Editability is per row: whatever `/meta/access` grants, the toolbar must
            // agree with it row by row.
            const writable = (path) => grants.write
                || (grants.scopes || []).some((s) => s.mode === 'rw'
                    && (path === s.prefix || path.startsWith(s.prefix + '.')));
            for (const [key, path] of [['host', 'traefik.spec.host'], ['groups', 'netbird.groups']]) {
                if (!rows2.some((r) => r.key === key)) continue;
                await L.clickRow(page2, key);
                const tb = await L.toolbarButtons(page2);
                const edit = tb.find((b) => b.label === 'Edit');
                check(`Edit is ${writable(path) ? 'enabled' : 'disabled'} for ${path}`,
                    !!edit && edit.disabled === !writable(path));
            }
            await page2.close();
        }

        check('no page errors', errors.length === 0, errors.join(' | '));
    } finally {
        await browser.close();
    }

    console.log(failures === 0 ? '\nALL OK' : `\n${failures} FAILED`);
    process.exit(failures === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });
