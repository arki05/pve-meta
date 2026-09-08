// meta-ui-dc-403-check.js - load the datacenter document as a zero-ACL scope holder
// (scoped@pve!t1), whose whole-document read 403s (F21). Read-only: no writes.
//
// Usage: node meta-ui-dc-403-check.js <host>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
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
        const page = await browser.newPage();
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 300) }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        // Log in as root (for cookie/CSRF bootstrap), then override outgoing API calls to
        // carry the scoped API token instead, so the page runs as scoped@pve!t1 against
        // /api2/json/meta/* without a real session for that principal.
        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
        await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
        await page.setRequestInterception(true);
        const reqLog = [];
        page.on('request', (req) => {
            const url = req.url();
            if (/\/api2\/json\/meta\//.test(url)) {
                const headers = Object.assign({}, req.headers());
                headers['authorization'] = 'PVEAPIToken=scoped@pve!t1=a23d85fc-e409-48a7-b8c8-3b9ba356aca3';
                headers['cookie'] = '';
                reqLog.push({ url: url.replace(/^https?:\/\/[^/]+/, ''), sentHeaders: headers });
                req.continue({ headers }).catch(() => {});
            } else {
                req.continue().catch(() => {});
            }
        });
        page.on('response', (r) => {
            if (/\/api2\/json\/meta\//.test(r.url())) {
                const e = reqLog.find((e) => e.url === r.url().replace(/^https?:\/\/[^/]+/, '') && e.status === undefined);
                if (e) e.status = r.status();
            }
        });

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?dc=1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await L.sleep(4000);

        out.body = (await page.evaluate(() => document.body.innerText)).slice(0, 500);
        out.hasMonaco = await page.evaluate(() => !!document.querySelector('.monaco-editor'));
        out.toolbarButtons = await L.toolbarButtons(page).catch(() => null);
        out.reqLog = reqLog;
        await page.screenshot({ path: `${OUT}/dc-403.png` });
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
