// Screenshots of the tree page for ui/docs/screenshots/.
//
//   node meta-ui-shots.js [host] [vmid]
//
// Writes to /root/headless/shots. The embedded shots drive the real PVE SPA (resource
// tree -> guest -> Metadata tab) so the page is photographed inside the tab it ships in;
// the rest are the standalone page, which is the same wasm bundle.
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = process.env.SHOTS || '/root/headless/shots-tree';

async function prep(page, t, theme) {
    await page.setViewport({ width: 1400, height: 900 });
    await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
    await page.setCookie({ name: 'PVEThemeCookie', value: theme === 'dark' ? 'proxmox-dark' : 'crisp', domain: host, path: '/', secure: true });
    await page.evaluateOnNewDocument((csrf, theme) => {
        try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {}
        try {
            localStorage.removeItem('ThemeMode');
            localStorage.removeItem('ThemeName');
        } catch (e) {}
        void theme;
    }, t.CSRFPreventionToken, theme);
    await page.emulateMediaFeatures([
        { name: 'prefers-color-scheme', value: theme === 'dark' ? 'dark' : 'light' },
    ]);
}

const standalone = (theme, query) =>
    `https://${host}:8006/pve2/js/pve-meta-ui/index.html?${query}&theme=${theme}`;

async function embedded(browser, t, theme) {
    // The PVE SPA occasionally comes up without its resource tree on a cold pveproxy;
    // the old checks retried, and so does this.
    let page;
    for (let attempt = 1; ; attempt++) {
        page = await browser.newPage();
        await prep(page, t, theme);
        await page.goto(`https://${host}:8006/`, { waitUntil: 'networkidle2', timeout: 60000 });
        const ok = await page
            .waitForFunction(() => document.querySelectorAll('.x-treelist-item-text').length > 0,
                { timeout: 20000, polling: 200 })
            .then(() => true).catch(() => false);
        if (ok) break;
        await page.close();
        if (attempt === 5) throw new Error('the PVE SPA never rendered its resource tree');
    }
    await L.sleep(1500);

    const target = await page.evaluateHandle((vmid) => new Promise((resolve, reject) => {
        const tree = Ext.ComponentQuery.query('treepanel')[0];
        const rec = tree.getStore().getNodeById(`lxc/${vmid}`);
        if (!rec) { reject(new Error('no such node: lxc/' + vmid)); return; }
        const ancestors = [];
        let p = rec.parentNode;
        while (p) { ancestors.unshift(p); p = p.parentNode; }
        let i = 0;
        (function next() {
            if (i >= ancestors.length) {
                requestAnimationFrame(() => setTimeout(() => resolve(tree.getView().getNode(rec)), 300));
                return;
            }
            const node = ancestors[i++];
            if (node.isExpanded()) next(); else node.expand(false, next);
        })();
    }), vmid);
    await target.asElement().click();
    await L.sleep(2500);

    const meta = await page.evaluateHandle(() =>
        Array.from(document.querySelectorAll('.x-treelist-item-text')).find((e) => e.textContent.trim() === 'Metadata'));
    await meta.asElement().click();
    await L.sleep(9000);

    await page.screenshot({ path: `${OUT}/embedded-${theme}.png` });
    const frameEl = await page.$('iframe[src*="/pve2/js/pve-meta-ui/"]');
    const frame = await frameEl.contentFrame();
    console.log(`embedded-${theme}: ` + (await frame.evaluate(() => document.body.innerText.replace(/\s+/g, ' ').slice(0, 120))));
    await page.close();
}

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    // A document that shows every row shape: nested maps, an array, a note, a string, and
    // (through the cluster's registrations) declared-but-unset rows.
    await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
        mode: 'replace',
        data: JSON.stringify({
            __: 'metadata for the lab container',
            // `port` is deliberately absent: the cluster's traefik grammar declares it,
            // so it shows as a declared-but-unset row with its default and a "set" action.
            traefik: { spec: { host: 'ct200.example' }, spec__: 'router definition' },
            netbird: { groups: ['lan', 'dmz'], groups__: 'peer groups this guest joins' },
            notes: 'a plain string value',
        }),
    });

    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium', headless: 'new',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
    });

    try {
        for (const theme of ['light', 'dark']) {
            await embedded(browser, t, theme);

            const page = await browser.newPage();
            await prep(page, t, theme);
            await page.goto(standalone(theme, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
            await L.waitForTree(page);
            await L.sleep(1500);
            await page.screenshot({ path: `${OUT}/standalone-${theme}.png` });
            await page.close();
        }

        // --- feature shots, light -------------------------------------------
        const page = await browser.newPage();
        await prep(page, t, 'light');
        await page.goto(standalone('light', `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
        await L.waitForTree(page);
        await L.sleep(1500);

        await page.screenshot({ path: `${OUT}/declared-rows.png` });

        await L.clickRow(page, 'host');
        await L.clickButton(page, 'Edit');
        await L.sleep(600);
        await page.screenshot({ path: `${OUT}/row-edit.png` });
        await page.keyboard.press('Escape');
        await L.sleep(600);

        await L.clickRow(page, 'spec');
        await L.clickButton(page, 'Add');
        await L.setField(page, 'Key', 'scheme');
        await L.sleep(400);
        await page.screenshot({ path: `${OUT}/add-row.png` });
        await page.keyboard.press('Escape');
        await L.sleep(600);

        await L.clickRow(page, 'spec');
        await L.clickButton(page, 'Edit as text');
        await L.sleep(2500);
        await page.screenshot({ path: `${OUT}/edit-as-text.png` });
        await L.clickButton(page, 'JSON', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(900);
        await page.screenshot({ path: `${OUT}/edit-as-text-json.png` });
        await L.clickButton(page, 'YAML', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(900);
        await L.editModel(page, 'ct200.example', 'diffed.example');
        await L.sleep(600);
        await L.clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(2000);
        await page.screenshot({ path: `${OUT}/diff-dialog.png` });
        await L.clickButton(page, 'Back', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(800);
        await page.keyboard.press('Escape');
        await L.sleep(800);

        // A 409: change the document from outside while a row dialog is open.
        await L.clickRow(page, 'host');
        await L.clickButton(page, 'Edit');
        await L.setField(page, 'Value', 'stale.example');
        await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
            mode: 'merge', data: JSON.stringify({ notes: 'changed by somebody else' }),
        });
        await L.sleep(9000);
        await L.clickButton(page, 'Update');
        await L.sleep(3000);
        await page.keyboard.press('Escape');
        await L.sleep(800);
        await page.screenshot({ path: `${OUT}/conflict-notice.png` });
        await page.close();

        // --- the datacenter document ----------------------------------------
        const dc = await browser.newPage();
        await prep(dc, t, 'light');
        await dc.goto(standalone('light', 'dc=1'), { waitUntil: 'networkidle2' });
        await L.sleep(4000);
        await dc.screenshot({ path: `${OUT}/datacenter.png` });
        console.log('datacenter: ' + (await dc.evaluate(() => document.body.innerText.replace(/\s+/g, ' ').slice(0, 200))));
        await dc.close();

        // --- a scoped principal (read-only where it has no rw scope) ---------
        // On a different guest: a scoped principal only has something to show where its
        // registration's selector actually matches, and the lab grants it `traefik` rw
        // (and nothing on `netbird`) on the second container.
        const roVmid = process.argv[4] || '201';
        const scoped = await L.ticket(host, 'scoped@pve', 'pvelabscoped').catch(() => null);
        if (scoped && scoped.ticket) {
            await L.api(host, t, 'PUT', `/meta/guests/${roVmid}`, {
                mode: 'replace',
                data: JSON.stringify({
                    traefik: { spec: { host: 'ct201.example' } },
                    netbird: { groups: ['lan'], groups__: 'peer groups this guest joins' },
                }),
            });
            const ro = await browser.newPage();
            await prep(ro, scoped, 'light');
            await ro.goto(standalone('light', `vmid=${roVmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
            await L.waitForTree(ro);
            await L.sleep(1200);
            // Whichever row this principal can actually see; the point of the shot is
            // that the toolbar follows the row's own grant.
            const visible = (await L.rows(ro)).map((r) => r.key);
            for (const key of ['groups', 'notes', 'host']) {
                if (visible.includes(key)) { await L.clickRow(ro, key); break; }
            }
            await L.sleep(500);
            await ro.screenshot({ path: `${OUT}/read-only.png` });
            await ro.close();
        } else {
            console.log('note: no scoped@pve login, skipping read-only.png');
        }
    } finally {
        await browser.close();
    }
    console.log('shots written to ' + OUT);
})().catch((e) => { console.error(e); process.exit(1); });
