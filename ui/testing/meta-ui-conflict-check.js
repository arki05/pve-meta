// meta-ui-conflict-check.js - the digest-conflict path: edit, let someone else write the
// document, then Apply. The server answers 409 and the page must raise the
// "changed on the server" notice with its Reload button, keeping the edit.
//
// Usage: node meta-ui-conflict-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';

const ORIGINAL = 'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups:\n  - lan\n';
const BEHIND_BACK = 'traefik:\n  spec:\n    host: ct200.example\n    port: 8080\nnetbird:\n  groups:\n  - lan\n';

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

async function write(t, text) {
    const get = await api(`/api2/json/meta/guests/${vmid}?format=yaml`, 'GET', null, t);
    const digest = JSON.parse(get.body).data.digest;
    return api(`/api2/json/meta/guests/${vmid}`, 'PUT', { mode: 'replace', text, digest }, t);
}

async function main() {
    const t = await ticket();
    const out = {};
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    try {
        await write(t, ORIGINAL);

        const page = await browser.newPage();
        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
        await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await sleep(2000);

        await page.evaluate(() => {
            const m = window.monaco.editor.getModels()[0];
            m.setValue(m.getValue().replace('ct200.example', 'ct200-conflict.example'));
        });
        await sleep(600);

        out.behindBack = await write(t, BEHIND_BACK);

        await page.evaluate(() => {
            const b = Array.from(document.querySelectorAll('button, .pwt-button'))
                .find((e) => e.textContent.trim() === 'Apply' && !e.disabled);
            b.click();
        });
        await page.waitForFunction(() => document.querySelectorAll('.monaco-diff-editor').length > 0, { timeout: 20000, polling: 300 });
        await sleep(1000);
        await page.evaluate(() => {
            const d = document.querySelector('.pwt-dialog:not(.pwt-dropdown)');
            const b = Array.from(d.querySelectorAll('button, .pwt-button')).find((e) => e.textContent.trim() === 'Apply');
            b.click();
        });
        await sleep(3500);

        out.bodyText = await page.evaluate(() => document.body.innerText.slice(0, 500));
        out.bannerAppeared = /changed on the server/i.test(out.bodyText);
        out.editorKeptEdit = await page.evaluate(() => window.monaco.editor.getModels()[0].getValue().includes('ct200-conflict'));
        await page.screenshot({ path: `${OUT}/conflict-banner.png` });

        // Reload from the banner: the editor must show the server's version again.
        await page.evaluate(() => {
            const b = Array.from(document.querySelectorAll('button, .pwt-button')).find((e) => e.textContent.trim() === 'Reload');
            if (b) b.click();
        });
        await sleep(3000);
        out.afterReload = await page.evaluate(() => window.monaco.editor.getModels()[0].getValue());
        await page.screenshot({ path: `${OUT}/after-conflict-reload.png` });
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }

    out.restore = await write(t, ORIGINAL);
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
