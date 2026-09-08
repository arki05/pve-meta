// meta-ui-r3-fix-check.js - live repro/verification for
// docs/REVIEW-2026-09-08-pass3.md R3 after the ui/ fix (editor.rs `view_outcome` /
// `refresh_views` / the whole-document poll digest + validated-empty-view load).
//
// Two runs against CT 200:
//   1. clean:  select `netbird`, delete the `netbird` key out from under the page via
//      the API, wait for the 5s version poll, assert the page falls back to the whole
//      document (selector leaves `netbird`, buffer is not left empty/stuck).
//   2. dirty:  same, but with an unapplied edit in the buffer first -- assert the draft
//      is NOT silently discarded (F5 discipline): either the selector/buffer are
//      untouched and a confirmation is offered, or the draft's own text survives into
//      whatever is shown.
//
// CT 200 is restored to its original document after each run.
//
// Usage: node meta-ui-r3-fix-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';

const ORIGINAL = 'traefik:\n  spec:\n    host: ct200.example\nnetbird:\n  groups:\n  - lan\n';
// `netbird` deleted entirely out from under the open `netbird` view.
const NETBIRD_REMOVED = 'traefik:\n  spec:\n    host: ct200.example\n';

async function restore(t) {
    const get = await L.api(host, t, 'GET', `/meta/guests/${vmid}?format=yaml`);
    const digest = JSON.parse(get.body).data.digest;
    return L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: ORIGINAL, digest });
}

async function withPage(browser, host, t, fn) {
    const page = await L.open(browser, host, t);
    const msgs = [];
    page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 200) }));
    page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));
    await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
        { waitUntil: 'networkidle2', timeout: 60000 });
    await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
    await L.sleep(2000);
    try {
        return await fn(page, msgs);
    } finally {
        await page.close();
    }
}

async function runClean(t) {
    const out = { name: 'clean (no draft)' };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });
    try {
        await withPage(browser, host, t, async (page, msgs) => {
            await L.pickView(page, 'netbird', 1500);
            out.beforeSelector = await L.selectorValue(page);
            out.beforeEditor = await L.readEditor(page);

            const get = await L.api(host, t, 'GET', `/meta/guests/${vmid}?format=yaml`);
            const digest = JSON.parse(get.body).data.digest;
            out.behindBack = await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: NETBIRD_REMOVED, digest });

            // 5s version poll + the fix's extra round trip(s); generous margin.
            await L.sleep(9000);

            out.afterSelector = await L.selectorValue(page);
            out.afterEditor = await L.readEditor(page);
            await page.screenshot({ path: `${OUT}/r3-clean-after.png` });

            out.verdict = (out.afterSelector !== 'netbird' && out.afterEditor !== null && out.afterEditor !== '{}\n' && out.afterEditor !== '')
                ? 'PASS: fell back to the whole document'
                : 'FAIL: still stuck on the vanished `netbird` view';
            out.console = msgs.filter((m) => m.type !== 'log').slice(0, 20);
        });
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    return out;
}

async function runDirty(t) {
    const out = { name: 'dirty (unsaved draft)' };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });
    try {
        await withPage(browser, host, t, async (page, msgs) => {
            await L.pickView(page, 'netbird', 1500);
            out.beforeSelector = await L.selectorValue(page);
            out.beforeEditor = await L.readEditor(page);

            // Make an unapplied edit -- never sent, must never be silently thrown away.
            await L.editModel(page, 'groups:', 'groups: # draft-marker-r3');
            await L.sleep(300);
            out.draftText = await L.readEditor(page);

            const get = await L.api(host, t, 'GET', `/meta/guests/${vmid}?format=yaml`);
            const digest = JSON.parse(get.body).data.digest;
            out.behindBack = await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: NETBIRD_REMOVED, digest });

            await L.sleep(9000);

            out.afterSelector = await L.selectorValue(page);
            out.afterEditor = await L.readEditor(page);
            out.modal = await L.modal(page);
            await page.screenshot({ path: `${OUT}/r3-dirty-after.png` });

            const draftSurvived = out.afterEditor === out.draftText;
            out.verdict = draftSurvived
                ? 'PASS: the draft is intact (' + (out.modal ? 'a confirmation is offered' : 'no modal, buffer just untouched') + ')'
                : 'FAIL: the draft was discarded (afterEditor differs from draftText)';
            out.console = msgs.filter((m) => m.type !== 'log').slice(0, 20);
        });
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    return out;
}

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const results = {};

    results.clean = await runClean(t);
    results.restoreAfterClean = await restore(t);

    results.dirty = await runDirty(t);
    results.restoreAfterDirty = await restore(t);

    console.log(JSON.stringify(results, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
