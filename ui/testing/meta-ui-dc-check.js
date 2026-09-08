// meta-ui-dc-check.js - the datacenter document asks `/meta/access?dc=1` and loads.
// Usage: node meta-ui-dc-check.js <host>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const out = { requests: [] };
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
        page.on('response', (r) => { if (/api2\/json\/meta/.test(r.url())) out.requests.push({ url: r.url().replace(/^https?:\/\/[^/]+/, ''), status: r.status() }); });

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?dc=1&theme=light`, { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        out.header = await page.evaluate(() => document.body.innerText.split('\n').slice(0, 3).join(' | '));
        out.text = await L.readEditor(page);
        out.readOnly = await L.editorReadOnly(page);
        out.buttons = await L.toolbarButtons(page);
        out.viewOptions = await (async () => {
            await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
            await L.sleep(900);
            const list = await page.evaluate(() => Array.from(document.querySelectorAll('.pwt-dialog.pwt-dropdown td')).map((e) => e.textContent.trim()));
            await page.keyboard.press('Escape');
            return list;
        })();
        out.console = msgs.filter((m) => m.type !== 'log').slice(0, 10);
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
