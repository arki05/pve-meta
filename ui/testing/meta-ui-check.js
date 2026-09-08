// meta-ui-check.js - headless verification of the rebuilt pve-meta editor page.
//
// Usage: node meta-ui-check.js <host> <vmid> [light|dark] [embedded|standalone|both]
//
// Adapted from ui-check.js: same ticket + cookie + Ext resource-tree navigation.
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const themeArg = process.argv[4] || 'light';
const modeArg = process.argv[5] || 'both';
const OUT = '/root/headless/shots';

function ticket() {
    return new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            {
                host,
                port: 8006,
                path: '/api2/json/access/ticket',
                method: 'POST',
                rejectUnauthorized: false,
                headers: {
                    'Content-Type': 'application/x-www-form-urlencoded',
                    'Content-Length': body.length,
                },
            },
            (r) => {
                let d = '';
                r.on('data', (c) => (d += c));
                r.on('end', () => {
                    try {
                        resolve(JSON.parse(d).data);
                    } catch (e) {
                        reject(new Error('ticket parse failed: ' + d));
                    }
                });
            },
        );
        req.on('error', reject);
        req.write(body);
        req.end();
    });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function frameReport(frame) {
    return frame.evaluate(() => {
        const q = (s) => document.querySelector(s);
        const monaco = q('.monaco-editor');
        const lines = document.querySelectorAll('.view-line');
        const cs = getComputedStyle(document.body);
        return {
            bodyText: document.body ? document.body.innerText.slice(0, 800) : '(no body)',
            html: document.documentElement.className,
            hasToolbar: !!q('.pwt-toolbar'),
            hasContentSpacer: !!q('.pwt-content-spacer'),
            hasMonaco: !!monaco,
            monacoLines: Array.from(lines).map((e) => e.textContent).slice(0, 20),
            monacoBg: monaco ? getComputedStyle(monaco).backgroundColor : null,
            fontFamily: cs.fontFamily,
            bodyBg: cs.backgroundColor,
            buttons: Array.from(document.querySelectorAll('button, .pwt-button')).map((b) => b.textContent.trim()).filter(Boolean),
        };
    });
}

async function main() {
    const t = await ticket();
    const result = {};
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1600,1000'],
        defaultViewport: { width: 1600, height: 1000 },
    });

    const prep = async (page) => {
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text() }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));
        page.on('requestfailed', (r) => msgs.push({ type: 'requestfailed', text: r.url() + ' ' + (r.failure() || {}).errorText }));
        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.setCookie({
            name: 'PVEThemeCookie',
            value: themeArg === 'dark' ? 'proxmox-dark' : 'crisp',
            domain: host,
            path: '/',
            secure: true,
        });
        await page.evaluateOnNewDocument((csrf) => {
            try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {}
            try { localStorage.removeItem('ThemeMode'); localStorage.removeItem('ThemeName'); } catch (e) {}
        }, t.CSRFPreventionToken);
        return msgs;
    };

    try {
        if (modeArg === 'both' || modeArg === 'embedded') {
            let page, msgs, lastErr;
            for (let attempt = 1; attempt <= 5; attempt++) {
                page = await browser.newPage();
                msgs = await prep(page);
                await page.goto(`https://${host}:8006/`, { waitUntil: 'networkidle2', timeout: 60000 });
                const ok = await page
                    .waitForFunction(() => document.querySelectorAll('.x-treelist-item-text').length > 0, { timeout: 15000, polling: 200 })
                    .then(() => true)
                    .catch((e) => { lastErr = e; return false; });
                if (ok) { await sleep(1000); break; }
                await page.close();
                if (attempt === 5) throw new Error('Datacenter view never rendered: ' + lastErr);
            }

            const targetHandle = await page.evaluateHandle((vmid) => new Promise((resolve, reject) => {
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
            const el = targetHandle.asElement();
            if (!el) throw new Error('no resource tree row for lxc/' + vmid);
            await el.click();
            await sleep(2000);

            const metaHandle = await page.evaluateHandle(() =>
                Array.from(document.querySelectorAll('.x-treelist-item-text')).find((e) => e.textContent.trim() === 'Metadata'));
            const metaEl = metaHandle.asElement();
            if (!metaEl) throw new Error('Metadata nav item not found');
            await metaEl.click();
            await sleep(9000);

            const iframeEl = await page.$('iframe[src*="/pve2/js/pve-meta-ui/"]');
            if (!iframeEl) throw new Error('no pve-meta-ui iframe');
            const frame = await iframeEl.contentFrame();
            const src = await page.evaluate((e) => e.getAttribute('src'), iframeEl);
            const report = await frameReport(frame);
            await page.screenshot({ path: `${OUT}/embedded-${themeArg}.png` });
            result.embedded = { iframeSrc: src, ...report, console: msgs.filter((m) => m.type !== 'log').slice(0, 20) };
            await page.close();
        }

        if (modeArg === 'both' || modeArg === 'standalone') {
            const page = await browser.newPage();
            const msgs = await prep(page);
            const url = `https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=${themeArg}`;
            await page.goto(url, { waitUntil: 'networkidle2', timeout: 60000 });
            await sleep(8000);
            const report = await frameReport(page.mainFrame());
            await page.screenshot({ path: `${OUT}/standalone-${themeArg}.png` });
            result.standalone = { url, ...report, console: msgs.filter((m) => m.type !== 'log').slice(0, 20) };
            await page.close();
        }
    } finally {
        await browser.close();
    }

    console.log(JSON.stringify(result, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
