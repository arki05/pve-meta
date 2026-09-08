// meta-ui-stale-check.js - the "changed on the server" path: with unapplied edits in
// the editor, change the document behind its back and let the 5s version poll notice.
//
// Usage: node meta-ui-stale-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';

function api(path, method, body, t) {
    return new Promise((resolve, reject) => {
        const data = body ? JSON.stringify(body) : null;
        const headers = { Cookie: 'PVEAuthCookie=' + encodeURIComponent(t.ticket) };
        if (data) {
            headers['Content-Type'] = 'application/json';
            headers['Content-Length'] = Buffer.byteLength(data);
            headers.CSRFPreventionToken = t.CSRFPreventionToken;
        }
        const req = https.request({ host, port: 8006, path, method, rejectUnauthorized: false, headers }, (r) => {
            let d = '';
            r.on('data', (c) => (d += c));
            r.on('end', () => resolve({ status: r.statusCode, body: d }));
        });
        req.on('error', reject);
        if (data) req.write(data);
        req.end();
    });
}

function ticket() {
    return new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            { host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
              headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length } },
            (r) => { let d = ''; r.on('data', (c) => (d += c)); r.on('end', () => { try { resolve(JSON.parse(d).data); } catch (e) { reject(new Error(d)); } }); },
        );
        req.on('error', reject); req.write(body); req.end();
    });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const ORIGINAL = 'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups:\n  - lan\n';
const BEHIND_BACK = 'traefik:\n  spec:\n    host: ct200.example\n    port: 8080\nnetbird:\n  groups:\n  - lan\n';

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
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 200) }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));
        page.on('response', (r) => { if (/meta\/version/.test(r.url())) msgs.push({ type: 'version', text: r.status() + '' }); });
        out.console = msgs;
        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
        await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await sleep(2000);

        // An unapplied edit in the editor.
        await page.evaluate(() => {
            const m = window.monaco.editor.getModels()[0];
            m.setValue(m.getValue() + '\nlocal:\n  note: unsaved\n');
        });
        await sleep(500);

        // Someone else writes the document.
        const get = await api(`/api2/json/meta/guests/${vmid}?format=yaml`, 'GET', null, t);
        const digest = JSON.parse(get.body).data.digest;
        out.behindBack = await api(`/api2/json/meta/guests/${vmid}`, 'PUT', { mode: 'replace', text: BEHIND_BACK, digest }, t);

        // Wait for the 5s version poll to notice.
        await page.waitForFunction(() => /changed on the server/i.test(document.body.innerText), { timeout: 20000, polling: 500 })
            .then(() => (out.bannerAppeared = true))
            .catch(() => (out.bannerAppeared = false));
        await sleep(500);
        await page.screenshot({ path: `${OUT}/stale-banner.png` });

        out.editorStillHasEdit = await page.evaluate(() => window.monaco.editor.getModels()[0].getValue().includes('unsaved'));
        out.bodyText = await page.evaluate(() => document.body.innerText.slice(0, 400));

        // Reload from the banner, and check the editor picked up the server's version.
        await page.evaluate(() => {
            const b = Array.from(document.querySelectorAll('button, .pwt-button')).find((e) => e.textContent.trim() === 'Reload');
            if (b) b.click();
        });
        await sleep(3000);
        out.afterReload = await page.evaluate(() => window.monaco.editor.getModels()[0].getValue());
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }

    // Put the document back.
    const get = await api(`/api2/json/meta/guests/${vmid}?format=yaml`, 'GET', null, t);
    const digest = JSON.parse(get.body).data.digest;
    out.restore = await api(`/api2/json/meta/guests/${vmid}`, 'PUT', { mode: 'replace', text: ORIGINAL, digest }, t);

    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
