// ui-write-check.js — exercise a write through the actual UI: change `host` in the
// Form view, click Apply, and report whether it succeeded (error banner text, if any).
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const newHost = process.argv[4] || 'ct200-changed.example';

function ticket() {
    return new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            {
                host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
                headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length },
            },
            (r) => {
                let d = '';
                r.on('data', (c) => (d += c));
                r.on('end', () => {
                    try { resolve(JSON.parse(d).data); } catch (e) { reject(new Error('ticket parse failed: ' + d)); }
                });
            },
        );
        req.on('error', reject);
        req.write(body);
        req.end();
    });
}
function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

async function main() {
    const t = await ticket();
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });
    const page = await browser.newPage();
    const consoleMsgs = [];
    page.on('console', (m) => consoleMsgs.push({ type: m.type(), text: m.text() }));
    page.on('pageerror', (e) => consoleMsgs.push({ type: 'pageerror', text: e.message }));

    await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
    await page.evaluateOnNewDocument((csrf) => {
        try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {}
    }, t.CSRFPreventionToken);

    await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}`, { waitUntil: 'networkidle2', timeout: 60000 });
    await sleep(6000);

    // Find the host input (first, and only, text input on this document) and set it.
    const before = await page.evaluate(() => document.querySelector('input')?.value);

    await page.evaluate((value) => {
        const input = document.querySelector('input');
        const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
        setter.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
        input.dispatchEvent(new Event('change', { bubbles: true }));
        input.blur();
    }, newHost);

    await sleep(500);

    const afterTyping = await page.evaluate(() => document.querySelector('input')?.value);
    const bodyTextBeforeApply = await page.evaluate(() => document.body.innerText);

    // Click "Apply" in the pending bar.
    const applyClicked = await page.evaluate(() => {
        const buttons = Array.from(document.querySelectorAll('button, [role="button"], span, div'));
        const btn = buttons.find((b) => b.textContent.trim() === 'Apply');
        if (!btn) return false;
        btn.click();
        return true;
    });

    await sleep(3000);

    const bodyTextAfterApply = await page.evaluate(() => document.body.innerText);

    await page.screenshot({ path: '/root/headless/ui-write-check.png' });
    await browser.close();

    console.log(JSON.stringify({
        before, afterTyping, applyClicked, bodyTextBeforeApply, bodyTextAfterApply, consoleMsgs,
    }, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
