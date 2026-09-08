// meta-ui-keys-stale-check.js - pass-3 UI dimension probe.
//
// Hypothesis: `self.keys` (the source of the "View as" selector's option list) is only
// refreshed on a whole-document (view="") load (docs/REVIEW-2026-09-08-pass2.md P10's
// fix, editor.rs `Loaded.keys` comment). A background reload (the 5s version-poll path,
// `Msg::ServerDigest` -> `send_reload()` when not dirty) reuses the *current* view, so if
// the user is sitting on a non-empty view when a top-level key is deleted out from under
// them, the stale `self.keys` still contains it, `options.contains(&id.view)` stays true,
// and editor.rs:592's "the selected view is gone, fall back to whole document" guard never
// fires.
//
// Usage: node meta-ui-keys-stale-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';

const ORIGINAL = 'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups:\n  - lan\n';
// `traefik` deleted entirely out from under the open `traefik` view.
const TRAEFIK_REMOVED = 'netbird:\n  groups:\n  - lan\n';

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
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 200) }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        // Switch to the `traefik` view (not dirty throughout this run).
        await L.pickView(page, 'traefik', 1500);
        out.beforeSelector = await L.selectorValue(page);
        out.beforeEditor = await L.readEditor(page);

        // Someone else deletes the `traefik` key entirely.
        const get = await L.api(host, t, 'GET', `/meta/guests/${vmid}?format=yaml`);
        const digest = JSON.parse(get.body).data.digest;
        out.behindBack = await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: TRAEFIK_REMOVED, digest });

        // Let the 5s version poll notice and auto-reload (not dirty -> no banner, straight
        // reload per editor.rs Msg::ServerDigest).
        await L.sleep(9000);

        out.afterSelector = await L.selectorValue(page);
        out.afterEditor = await L.readEditor(page);
        out.afterOptions = await page.evaluate(() =>
            Array.from(document.querySelectorAll('.pwt-toolbar')).length ? true : true);
        await page.screenshot({ path: `${OUT}/keys-stale-after.png` });

        // Does the "View as" list still offer `traefik`, or did it correctly disappear /
        // fall back?
        await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
        await L.sleep(900);
        out.viewOptionsAfterDeletion = await page.evaluate(() =>
            Array.from(document.querySelectorAll('.pwt-dialog.pwt-dropdown td, .pwt-dialog.pwt-dropdown [role="row"]'))
                .map((e) => e.textContent.trim()));
        await page.keyboard.press('Escape');
        await L.sleep(300);

        out.verdict = (out.afterSelector === 'traefik' && out.afterEditor === '')
            ? 'STALE: still on the deleted `traefik` view with an empty buffer, selector still shows traefik, no fallback to whole document'
            : (out.afterSelector !== 'traefik')
                ? 'clean: fell back to whole document'
                : 'unexpected state, see afterSelector/afterEditor';

        out.console = msgs.filter((m) => m.type !== 'log').slice(0, 20);
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }

    // Restore.
    try {
        const get = await L.api(host, t, 'GET', `/meta/guests/${vmid}?format=yaml`);
        out.restoreGet = get;
        const digest = JSON.parse(get.body).data.digest;
        out.restore = await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: ORIGINAL, digest });
    } catch (e) {
        out.restoreError = String(e);
    }

    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
