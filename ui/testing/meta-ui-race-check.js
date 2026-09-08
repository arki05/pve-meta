// meta-ui-race-check.js - reproduces REVIEW-2026-09-07 F4 deliberately.
//
// A view switch is made while a slow load is in flight: the GET for `view=traefik` is
// held back with request interception, the page is switched back to the whole document
// while it is still outstanding, and the late answer then arrives. Before the fix, that
// answer was applied unconditionally, so the whole-document view ended up showing (and
// Apply would have written) the `traefik` subtree.
//
// Usage: node meta-ui-race-check.js <host> <vmid> <delayMs> <label>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const delayMs = parseInt(process.argv[4] || '9000', 10);
const label = process.argv[5] || 'run';
const OUT = '/root/headless/shots';

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const out = { label, delayMs, requests: [] };
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

        const t0 = Date.now();
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const url = req.url();
            const slow = /api2\/json\/meta/.test(url) && /view=traefik/.test(url);
            if (slow) {
                out.requests.push({ at: Date.now() - t0, held: delayMs, url: url.replace(/^https?:\/\/[^/]+/, '') });
                setTimeout(() => req.continue().catch(() => {}), delayMs);
            } else {
                if (/api2\/json\/meta/.test(url)) out.requests.push({ at: Date.now() - t0, url: url.replace(/^https?:\/\/[^/]+/, '') });
                req.continue().catch(() => {});
            }
        });
        page.on('response', (r) => {
            if (/api2\/json\/meta/.test(r.url())) {
                const e = out.requests.find((e) => e.url === r.url().replace(/^https?:\/\/[^/]+/, '') && e.done === undefined);
                if (e) { e.done = Date.now() - t0; e.status = r.status(); }
            }
        });

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        out.wholeDocument = await L.readEditor(page);

        // Switch to `traefik` (its GET is held), then straight back to the whole document
        // before the held answer can arrive.
        await L.pickView(page, 'traefik', 600);
        out.afterSwitchToTraefik = { at: Date.now() - t0, editor: await L.readEditor(page), selector: await L.selectorValue(page) };

        await L.pickView(page, 'Whole document', 600);
        out.afterSwitchBack = { at: Date.now() - t0, editor: await L.readEditor(page), selector: await L.selectorValue(page) };

        // Now let the held answer land.
        await L.sleep(delayMs + 5000);

        out.final = { at: Date.now() - t0, editor: await L.readEditor(page), selector: await L.selectorValue(page) };
        out.misattributed = out.final.editor !== out.wholeDocument;
        out.verdict = out.misattributed
            ? 'MISATTRIBUTED: the whole-document view shows the answer to a `view=traefik` request'
            : 'clean: the late answer for `view=traefik` was dropped';
        await page.screenshot({ path: `${OUT}/race-${label}.png` });

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
