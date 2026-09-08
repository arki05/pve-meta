// meta-ui-empty-doc-check.js - load a guest with no meta document at all (vmid 300,
// qemu, no /etc/pve/meta/guests/300.yaml on disk) as root. Read-only: no writes.
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '300';
const OUT = '/root/headless/shots';

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const out = {};
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });
    try {
        const page = await L.open(browser, host, t);
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 300) }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=qemu&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        out.editor = await L.readEditor(page);
        out.selector = await L.selectorValue(page);
        out.toolbarButtons = await L.toolbarButtons(page);
        out.headerText = await page.evaluate(() => document.body.innerText.split('\n').slice(0, 6).join(' | '));

        await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
        await L.sleep(900);
        out.viewOptions = await page.evaluate(() =>
            Array.from(document.querySelectorAll('.pwt-dialog.pwt-dropdown td, .pwt-dialog.pwt-dropdown [role="row"]'))
                .map((e) => e.textContent.trim()));
        await page.keyboard.press('Escape');
        await page.screenshot({ path: `${OUT}/empty-doc-300.png` });
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
