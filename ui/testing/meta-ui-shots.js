// Screenshots of the tree page for ui/docs/screenshots/.
//
//   node meta-ui-shots.js [host] [vmid]
//
// Writes to /root/headless/shots-tree. Five shots in each theme (`docs/DESIGN.md`
// section 8): the tree, the Text body, the "Edit selection as text" dialog, the Access
// tooltip, and the page inside the real PVE tab. The embedded shots drive the actual PVE
// SPA (resource tree -> guest -> Metadata tab); the rest are the standalone page, which
// is the same wasm bundle.
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib.js');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = process.env.SHOTS || '/root/headless/shots-tree';

async function prep(page, t, theme) {
    await page.setViewport({ width: 1400, height: 700 });
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
    await seed(t);
    // The PVE SPA occasionally comes up without its resource tree on a cold pveproxy;
    // the old checks retried, and so does this.
    let page;
    for (let attempt = 1; ; attempt++) {
        page = await browser.newPage();
        await prep(page, t, theme);
        await page.setViewport({ width: 1400, height: 900 });
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
    console.log(`embedded-${theme}: ` + (await frame.evaluate(() => document.body.innerText.replace(/\s+/g, ' ').slice(0, 160))));
    await page.close();
}

// The document every shot is taken of: nested maps, an array, notes, a string, and
// (through the cluster's registrations) a declared-but-unset row. Re-seeded before each
// capture, so a run stays reproducible even when something else on the lab is writing.
const seed = (t) => L.api(host, t, 'PUT', `/meta/guests/${vmid}`, {
    mode: 'replace',
    data: JSON.stringify({
        __: 'metadata for the lab container',
        // `port` is deliberately absent: the cluster's traefik grammar declares it, so it
        // shows as a declared-but-unset row, greyed, with its default.
        traefik: { spec: { host: 'ct200.example' }, spec__: 'router definition' },
        netbird: { groups: ['lan', 'dmz'], groups__: 'peer groups this guest joins' },
        notes: 'a plain string value',
    }),
});

// The four standalone shots of one theme.
async function panel(browser, t, theme) {
    await seed(t);
    const page = await browser.newPage();
    await prep(page, t, theme);
    await page.goto(standalone(theme, `vmid=${vmid}&type=lxc&node=pvemeta-node1`), { waitUntil: 'networkidle2' });
    await L.waitForTree(page);
    await L.sleep(1500);

    // 1. the tree, with a row selected so the toolbar is at full strength
    await L.clickRow(page, 'host');
    await L.sleep(400);
    await page.screenshot({ path: `${OUT}/tree-${theme}.png` });

    // 2. the Access tooltip: hover the cell and wait pwt's 1 s dwell out
    const tip = await L.hoverTip(page, 'netbird', 3);
    console.log(`access tooltip (${theme}): ` + JSON.stringify(tip));
    await page.screenshot({ path: `${OUT}/access-tooltip-${theme}.png` });
    await page.mouse.move(700, 560);
    await L.sleep(600);

    // 3. "Edit selection as text" on the selected subtree
    await L.clickRow(page, 'spec');
    await L.clickButton(page, 'Edit selection as text');
    await L.sleep(2800);
    // The dialog autofocuses its close tool; a focus ring on it is not the chrome.
    await page.evaluate(() => document.activeElement && document.activeElement.blur());
    await L.sleep(300);
    await page.screenshot({ path: `${OUT}/selection-dialog-${theme}.png` });
    await page.keyboard.press('Escape');
    await L.sleep(900);

    // 4. the Text body of the Tree | Text toggle
    await L.clickButton(page, 'Text');
    await L.sleep(3200);
    await page.screenshot({ path: `${OUT}/text-${theme}.png` });
    await page.close();
}

(async () => {
    const t = await L.ticket(host, 'root@pam', 'pvelab');

    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium', headless: 'new',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
    });

    try {
        for (const theme of ['light', 'dark']) {
            await panel(browser, t, theme);
            await embedded(browser, t, theme);
        }
    } finally {
        await browser.close();
    }
    console.log('shots written to ' + OUT);
})().catch((e) => { console.error(e); process.exit(1); });
