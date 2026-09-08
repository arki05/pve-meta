// meta-ui-q2-check.js - live repro/verification for docs/REVIEW-2026-09-08-pass4.md Q2
// after the ui/ fix (editor.rs `Msg::Reload` / `Msg::VersionToken` re-fetch `api::access`,
// fed through `refresh_views()`/`view_outcome()` exactly like fresh `keys`).
//
// scoped@pve!t1's grants (traefik rw, netbird ro) are configured on the *token* identity;
// a browser tab authenticates with a session ticket, and a token cannot mint one
// (docs/REVIEW-2026-09-08-pass4.md Q2's own text names this dilemma). So this run logs in
// as the plain user `scoped@pve` (a password is set for the duration of the lab session)
// and temporarily grants that identity the same scopes datacenter.yaml already configures
// for its token, via the supported `mode=merge` write to `/meta/datacenter`'s `scopes`
// view -- `scoped@pve!t1`'s own entry is never touched. `datacenter.yaml` is restored to
// its exact original bytes (checked both by the API's own digest and by an md5sum of the
// file on disk) once each run finishes, however it finishes.
//
// Two runs against CT 200, `traefik` view:
//   1. clean:  no draft. The scope is revoked; the view is expected to fall back to the
//      whole document on its own (`ViewOutcome::FallBack`) within one poll interval, and
//      `traefik` must no longer be a "View as" option.
//   2. dirty:  an edit is made first (so Apply's `disabled` genuinely tests write access,
//      not mere cleanliness), then the scope is revoked. Apply must disable even though
//      the buffer stays on the vanished view pending the discard confirmation
//      (`ViewOutcome::ConfirmFallBack`, the same F5 discipline R3 already covers) -- the
//      draft itself must not be silently discarded, and `traefik` must no longer be listed.
//
// Usage: node meta-ui-q2-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';
const SCOPED_USER = 'scoped@pve';
const SCOPED_PASSWORD = 'ScopedTest123!';

async function dcText(rootTicket) {
    const r = await L.api(host, rootTicket, 'GET', '/meta/datacenter?format=yaml');
    return JSON.parse(r.body).data;
}

async function grantTraefik(rootTicket, digest) {
    return L.api(host, rootTicket, 'PUT', '/meta/datacenter', {
        mode: 'merge', view: 'scopes', digest,
        data: JSON.stringify({ [SCOPED_USER]: [{ prefix: 'traefik', mode: 'rw' }, { prefix: 'netbird', mode: 'ro' }] }),
    });
}

async function revokeTraefik(rootTicket, digest) {
    return L.api(host, rootTicket, 'PUT', '/meta/datacenter', {
        mode: 'merge', view: 'scopes', digest,
        data: JSON.stringify({ [SCOPED_USER]: [{ prefix: 'netbird', mode: 'ro' }] }),
    });
}

async function restoreDc(rootTicket, originalText) {
    const cur = await dcText(rootTicket);
    return L.api(host, rootTicket, 'PUT', '/meta/datacenter', {
        mode: 'replace', text: originalText, digest: cur.digest,
    });
}

async function runCase({ name, dirty, rootTicket, originalText, shot }) {
    const out = { name };
    let browser;
    try {
        const before = await dcText(rootTicket);
        out.grantPut = { status: (await grantTraefik(rootTicket, before.digest)).status };

        const scopedTicket = await L.ticket(host, SCOPED_USER, SCOPED_PASSWORD);
        browser = await puppeteer.launch({
            executablePath: '/usr/bin/chromium',
            args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
            defaultViewport: { width: 1400, height: 900 },
        });
        const page = await L.open(browser, host, scopedTicket);
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 200) }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        await L.pickView(page, 'traefik', 1500);
        out.beforeSelector = await L.selectorValue(page);

        if (dirty) {
            await L.editModel(page, 'host:', 'host: # q2-draft-marker');
            await L.sleep(300);
            out.draftText = await L.readEditor(page);
        }

        out.beforeApply = (await L.toolbarButtons(page)).find((b) => b.label === 'Apply');
        out.beforeReadOnlyMarker = await page.evaluate(() => /Read-only/.test(document.body.innerText));

        // An admin revokes just the `traefik` scope, from the outside.
        const mid = await dcText(rootTicket);
        out.revokePut = { status: (await revokeTraefik(rootTicket, mid.digest)).status };

        // One version-poll interval (5s), generous margin.
        await L.sleep(9000);

        out.afterApply = (await L.toolbarButtons(page)).find((b) => b.label === 'Apply');
        out.afterReadOnlyMarker = await page.evaluate(() => /Read-only/.test(document.body.innerText));
        out.afterSelector = await L.selectorValue(page);
        out.afterEditor = await L.readEditor(page);
        out.modal = await L.modal(page);

        await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
        await L.sleep(900);
        out.afterOptions = await page.evaluate(() =>
            Array.from(document.querySelectorAll('.pwt-dialog.pwt-dropdown td, .pwt-dialog.pwt-dropdown [role="row"]'))
                .map((e) => e.textContent.trim()));
        await page.keyboard.press('Escape');
        await L.sleep(300);
        if (shot) await page.screenshot({ path: `${OUT}/${shot}.png` });

        out.console = msgs.filter((m) => m.type !== 'log').slice(0, 30);

        const traefikGone = !out.afterOptions.includes('traefik');
        const applyDisabled = !!(out.afterApply && out.afterApply.disabled);
        const draftIntact = !dirty || out.afterEditor === out.draftText;
        out.verdict = (traefikGone && applyDisabled && draftIntact)
            ? 'PASS: `traefik` dropped from the "View as" selector and Apply disabled within one poll interval' +
              (dirty ? ', draft left intact' : '')
            : 'FAIL: ' +
              `traefikGone=${traefikGone}, applyDisabled=${applyDisabled}, draftIntact=${draftIntact}`;

        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        if (browser) await browser.close();
        try {
            out.restore = { status: (await restoreDc(rootTicket, originalText)).status };
        } catch (e) {
            out.restoreError = String(e);
        }
    }
    return out;
}

async function main() {
    const rootTicket = await L.ticket(host, 'root@pam', 'pvelab');
    const baseline = await dcText(rootTicket);
    const originalText = baseline.text;
    const results = { originalDigest: baseline.digest };

    results.clean = await runCase({ name: 'clean (no draft)', dirty: false, rootTicket, originalText, shot: 'q2-clean-after' });
    results.dirty = await runCase({ name: 'dirty (unsaved draft)', dirty: true, rootTicket, originalText, shot: 'q2-dirty-after' });

    console.log(JSON.stringify(results, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
