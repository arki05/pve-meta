// ui-check.js - headless verification that the pve-meta editor UI (wasm app)
// actually renders inside the "Metadata" tab iframe on node1's real pve-manager
// SPA, and also when loaded standalone at the same URL the iframe points at.
//
// Usage: node ui-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';

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

function sleep(ms) {
    return new Promise((r) => setTimeout(r, ms));
}

async function collectFrameChecks(frame, consoleMsgs) {
    const text = await frame.evaluate(() => document.body ? document.body.innerText : '(no body)');
    const hasTraefik = /traefik/i.test(text);
    const hasHost = text.includes('ct200.example');
    const notLoggedIn = /not logged in/i.test(text);
    return { text, hasTraefik, hasHost, notLoggedIn, consoleErrors: consoleMsgs.filter((m) => m.type === 'error') };
}

async function main() {
    const result = { embedded: {}, standalone: {} };
    const t = await ticket();

    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    try {
        // ---------- Embedded: real PVE SPA, CT 200, Metadata tab ----------
        {
            // Known pre-existing pve-manager flake (see pve-manager-patch/testing/check-tab.js
            // header comment): the initial Datacenter view can race with GuiCap and throw,
            // leaving the nav treelist empty. Retry with fresh pages, bounded, same as that script.
            let page, consoleMsgs, responses;
            const maxAttempts = 5;
            let lastErr;
            for (let attempt = 1; attempt <= maxAttempts; attempt++) {
                page = await browser.newPage();
                consoleMsgs = [];
                responses = [];
                page.on('console', (m) => consoleMsgs.push({ type: m.type(), text: m.text(), url: page.url() }));
                page.on('response', (r) => {
                    if (/pve2\/js\/pve-meta-ui|api2\/json\/meta/.test(r.url())) {
                        responses.push({ url: r.url(), status: r.status() });
                    }
                });
                page.on('pageerror', (e) => consoleMsgs.push({ type: 'pageerror', text: e.message, url: page.url() }));

                await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
                await page.evaluateOnNewDocument((csrf) => {
                    try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {}
                }, t.CSRFPreventionToken);

                await page.goto(`https://${host}:8006/`, { waitUntil: 'networkidle2', timeout: 60000 });
                const ok = await page
                    .waitForFunction(() => document.querySelectorAll('.x-treelist-item-text').length > 0, { timeout: 15000, polling: 200 })
                    .then(() => true)
                    .catch((e) => { lastErr = e; return false; });
                if (ok) { await sleep(1000); break; }
                await page.close();
                if (attempt === maxAttempts) throw new Error('initial Datacenter view never rendered after ' + maxAttempts + ' attempts: ' + lastErr);
            }

            // Select CT 200 in the resource tree.
            const targetHandle = await page.evaluateHandle((vmid) => {
                return new Promise((resolve, reject) => {
                    const tree = Ext.ComponentQuery.query('treepanel')[0];
                    const store = tree.getStore();
                    const rec = store.getNodeById(`lxc/${vmid}`);
                    if (!rec) { reject(new Error('no such node in resource tree: lxc/' + vmid)); return; }
                    const ancestors = [];
                    let p = rec.parentNode;
                    while (p) { ancestors.unshift(p); p = p.parentNode; }
                    let i = 0;
                    function expandNext() {
                        if (i >= ancestors.length) {
                            requestAnimationFrame(() => setTimeout(() => resolve(tree.getView().getNode(rec)), 300));
                            return;
                        }
                        const node = ancestors[i++];
                        if (node.isExpanded()) expandNext(); else node.expand(false, expandNext);
                    }
                    expandNext();
                });
            }, vmid);
            const targetEl = targetHandle.asElement();
            if (!targetEl) throw new Error('could not resolve resource tree row for lxc/' + vmid);
            await targetEl.click();
            await sleep(2000);

            const metaHandle = await page.evaluateHandle(() =>
                Array.from(document.querySelectorAll('.x-treelist-item-text')).find((e) => e.textContent.trim() === 'Metadata'),
            );
            const metaEl = metaHandle.asElement();
            if (!metaEl) throw new Error('Metadata nav item not found for CT ' + vmid);
            await metaEl.click();

            // Wait ~8s for the wasm app to boot inside the iframe.
            await sleep(8000);

            const iframeEl = await page.$('iframe[src*="/pve2/js/pve-meta-ui/"]');
            if (!iframeEl) throw new Error('no pve-meta-ui iframe found after clicking Metadata');
            const frame = await iframeEl.contentFrame();
            if (!frame) throw new Error('could not get contentFrame() for the pve-meta-ui iframe');

            const src = await page.evaluate((el) => el.getAttribute('src'), iframeEl);
            const checks = await collectFrameChecks(frame, consoleMsgs.filter((m) => m.url === frame.url() || true));

            await page.screenshot({ path: '/root/headless/ui-embedded.png' });

            result.embedded = {
                iframeSrc: src,
                frameUrl: frame.url(),
                bodyText: checks.text,
                hasTraefik: checks.hasTraefik,
                hasHost: checks.hasHost,
                notLoggedIn: checks.notLoggedIn,
                allConsole: consoleMsgs,
                trackedResponses: responses,
            };
            await page.close();
        }

        // ---------- Standalone: direct URL, cookie set, same origin ----------
        {
            const page = await browser.newPage();
            const consoleMsgs = [];
            const responses = [];
            page.on('console', (m) => consoleMsgs.push({ type: m.type(), text: m.text() }));
            page.on('pageerror', (e) => consoleMsgs.push({ type: 'pageerror', text: e.message }));
            page.on('response', (r) => {
                if (/pve2\/js\/pve-meta-ui|api2\/json\/meta/.test(r.url())) {
                    responses.push({ url: r.url(), status: r.status() });
                }
            });

            await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
            await page.evaluateOnNewDocument((csrf) => {
                try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {}
            }, t.CSRFPreventionToken);

            const url = `https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}`;
            await page.goto(url, { waitUntil: 'networkidle2', timeout: 60000 });
            await sleep(8000);

            const text = await page.evaluate(() => document.body ? document.body.innerText : '(no body)');
            await page.screenshot({ path: '/root/headless/ui-standalone.png' });

            result.standalone = {
                url,
                bodyText: text,
                hasTraefik: /traefik/i.test(text),
                hasHost: text.includes('ct200.example'),
                notLoggedIn: /not logged in/i.test(text),
                allConsole: consoleMsgs,
                trackedResponses: responses,
            };
            await page.close();
        }
    } finally {
        await browser.close();
    }

    console.log(JSON.stringify(result, null, 2));
}

main().catch((e) => {
    console.error('FATAL', e);
    process.exit(1);
});
