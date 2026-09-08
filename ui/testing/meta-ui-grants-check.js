// meta-ui-grants-check.js - REVIEW-2026-09-07 F22: the editor's read-only state comes
// from `GET /meta/access?vmid=<id>`'s *write* half, not from an audit-derived `full`.
//
// The page's session is root's (the lab has no unprivileged user whose password is known
// here), so the grants themselves are stubbed at the transport: the answer to
// `/meta/access?vmid=...` is replaced with the one the API returns for the principal being
// simulated, and the page's own reaction to it is what is asserted. The grants used are
// the ones the datacenter document actually configures for `scoped@pve!t1`
// (traefik rw, netbird ro) plus the pure-auditor case (read, no write, no scopes).
//
// Usage: node meta-ui-grants-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';

const CASES = [
    {
        name: 'auditor (VM.Audit, no VM.Config.Options)',
        grants: { read: 1, write: 0, scopes: [] },
        views: [''],
        shot: 'read-only',
    },
    {
        name: 'scoped principal (traefik rw, netbird ro), as configured for scoped@pve!t1',
        grants: { read: 0, write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }, { prefix: 'netbird', mode: 'ro' }] },
        views: ['', 'traefik', 'netbird'],
    },
    {
        name: 'full write (root)',
        grants: { read: 1, write: 1, scopes: [] },
        views: [''],
    },
];

async function run(browser, t, testCase) {
    const page = await L.open(browser, host, t);
    const out = { case: testCase.name, grants: testCase.grants, views: [] };

    await page.setRequestInterception(true);
    page.on('request', (req) => {
        if (/api2\/json\/meta\/access/.test(req.url())) {
            out.accessUrl = req.url().replace(/^https?:\/\/[^/]+/, '');
            req.respond({
                status: 200,
                contentType: 'application/json',
                body: JSON.stringify({ data: testCase.grants }),
            }).catch(() => {});
            return;
        }
        req.continue().catch(() => {});
    });

    await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
        { waitUntil: 'networkidle2', timeout: 60000 });
    await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
    await L.sleep(2000);

    for (const view of testCase.views) {
        if (view) await L.pickView(page, view, 2500);
        const state = {
            view: view || 'whole document',
            selector: await L.selectorValue(page),
            editorReadOnly: await L.editorReadOnly(page),
            readOnlyMarker: await page.evaluate(() => /Read-only/.test(document.body.innerText)),
            apply: (await L.toolbarButtons(page)).find((b) => b.label === 'Apply'),
        };
        // With something to apply, Apply must still be disabled where writing is not
        // allowed - that is the whole of F22.
        await page.evaluate(() => {
            const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
            const model = e.getModel();
            model.setValue(model.getValue() + 'draft__: touched\n');
        });
        await L.sleep(700);
        state.applyWithDraft = (await L.toolbarButtons(page)).find((b) => b.label === 'Apply');
        state.dirty = (await L.readEditor(page)).includes('draft__');
        out.views.push(state);
        // Drop the draft again so the next view switch does not need confirming.
        await L.clickButton(page, 'Discard').catch(() => {});
        await L.sleep(400);
        await L.clickButton(page, 'Yes', '.pwt-dialog:not(.pwt-dropdown)').catch(() => {});
        await L.sleep(900);
    }

    if (testCase.shot) await page.screenshot({ path: `${OUT}/${testCase.shot}.png` });
    await page.close();
    return out;
}

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const out = { cases: [] };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });
    try {
        for (const testCase of CASES) {
            out.cases.push(await run(browser, t, testCase));
        }
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
