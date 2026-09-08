// meta-ui-ro-check.js - a principal with only a read-only scope must get a read-only
// editor, a disabled Apply, and only the views its scope covers.
//
// Usage: node meta-ui-ro-check.js <host> <vmid> <user> <password>
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const user = process.argv[4] || 'svc@pve';
const pass = process.argv[5] || 'pvelabsvc';
const OUT = '/root/headless/shots';

function ticket() {
    return new Promise((resolve, reject) => {
        const body = `username=${encodeURIComponent(user)}&password=${encodeURIComponent(pass)}`;
        const req = https.request(
            { host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
              headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length } },
            (r) => { let d = ''; r.on('data', (c) => (d += c)); r.on('end', () => { try { resolve(JSON.parse(d).data); } catch (e) { reject(new Error(d)); } }); },
        );
        req.on('error', reject); req.write(body); req.end();
    });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
    const t = await ticket();
    const out = {};
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    try {
        const page = await browser.newPage();
        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
        await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await sleep(2500);

        out.bodyText = await page.evaluate(() => document.body.innerText.slice(0, 400));
        out.editorReadOnly = await page.evaluate(() => window.monaco.editor.getEditors()[0].getOption(window.monaco.editor.EditorOption.readOnly));
        out.buttons = await page.evaluate(() =>
            Array.from(document.querySelectorAll('button, .pwt-button'))
                .map((b) => ({ label: b.textContent.trim(), disabled: !!b.disabled || b.getAttribute('aria-disabled') === 'true' }))
                .filter((b) => b.label));

        // The picker must offer only what the scope covers.
        await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
        await sleep(800);
        out.viewOptions = await page.evaluate(() =>
            Array.from(document.querySelectorAll('.pwt-dialog.pwt-dropdown td')).map((e) => e.textContent.trim()));
        await page.keyboard.press('Escape');
        await sleep(400);
        await page.screenshot({ path: `${OUT}/read-only.png` });
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
