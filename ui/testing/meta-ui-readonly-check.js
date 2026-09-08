// meta-ui-readonly-check.js - REVIEW-2026-09-07 F22: read and write are separate grants.
//
// A principal that may read a document but not write it (VM.Audit without
// VM.Config.Options - or an `ro` scope) must get a read-only Monaco, a disabled Apply and
// the "Read-only" marker, rather than an editable buffer and a 403 after the diff.
//
// Usage: node meta-ui-readonly-check.js <host> <vmid> <user> <password>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const user = process.argv[4] || 'scoped@pve';
const pass = process.argv[5] || 'pvelabscoped';
const OUT = '/root/headless/shots';

async function main() {
    const t = await L.ticket(host, user, pass);
    const out = { user };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    try {
        const page = await L.open(browser, host, t);
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text() }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        const access = [];
        page.on('response', (r) => { if (/meta\/access/.test(r.url())) access.push({ url: r.url().replace(/^https?:\/\/[^/]+/, ''), status: r.status() }); });

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2500);

        out.accessRequests = access;
        out.text = await L.readEditor(page);
        out.editorReadOnly = await L.editorReadOnly(page);
        out.buttons = await L.toolbarButtons(page);
        out.readOnlyMarker = await page.evaluate(() => /Read-only/.test(document.body.innerText));

        // Even with a draft forced into the model, Apply must stay disabled.
        await L.editModel(page, 'ct200', 'ct200-nope');
        await L.sleep(900);
        out.buttonsWithDraft = await L.toolbarButtons(page);

        await page.screenshot({ path: `${OUT}/read-only.png` });
        out.console = msgs.filter((m) => m.type !== 'log').slice(0, 20);
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
