// meta-ui-draft-check.js - REVIEW-2026-09-07 F5: unapplied edits are never thrown away
// without the confirmation the Discard button uses.
//
// Walks the three ways an edit could be lost: the toolbar Reload, a "View as" switch, and
// an external change picked up by the version poll.
//
// Usage: node meta-ui-draft-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';
const DRAFT = 'ct200-draft.example';

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const out = { steps: [] };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    let original = null;
    try {
        const page = await L.open(browser, host, t);
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text() }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        original = await L.readEditor(page);
        out.steps.push({ step: 'loaded', text: original, buttons: await L.toolbarButtons(page), readOnly: await L.editorReadOnly(page) });

        // --- 1. Reload with a draft ------------------------------------------
        await L.editModel(page, 'ct200.example', DRAFT);
        await L.sleep(800);
        out.steps.push({ step: 'edited', dirty: (await L.readEditor(page)).includes(DRAFT), buttons: await L.toolbarButtons(page) });

        await L.clickAriaButton(page, 'Refresh');
        await L.sleep(900);
        out.steps.push({ step: 'reload with a draft asks first', modal: await L.modal(page) });
        await page.screenshot({ path: `${OUT}/reload-confirm.png` });

        await L.clickButton(page, 'No', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(1200);
        out.steps.push({ step: 'reload declined', keptDraft: (await L.readEditor(page)).includes(DRAFT), modal: await L.modal(page) });

        // --- 2. View switch with a draft --------------------------------------
        await L.pickView(page, 'traefik', 900);
        out.steps.push({ step: 'view switch with a draft asks first', modal: await L.modal(page) });
        await page.screenshot({ path: `${OUT}/switch-confirm.png` });

        await L.clickButton(page, 'No', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(1500);
        out.steps.push({
            step: 'view switch declined',
            keptDraft: (await L.readEditor(page)).includes(DRAFT),
            selector: await L.selectorValue(page),
            modal: await L.modal(page),
        });

        await L.pickView(page, 'traefik', 900);
        await L.clickButton(page, 'Yes', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(3000);
        out.steps.push({ step: 'view switch confirmed', selector: await L.selectorValue(page), text: await L.readEditor(page) });

        await L.pickView(page, 'Whole document', 3000);
        out.steps.push({ step: 'back to the whole document', selector: await L.selectorValue(page), text: await L.readEditor(page) });

        // --- 3. An external change while a draft is open -----------------------
        await L.editModel(page, 'ct200.example', DRAFT);
        await L.sleep(800);
        const external = original.replace('ct200.example', 'ct200-external.example');
        const put = await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: external });
        out.steps.push({ step: 'changed on the server from outside', status: put.status, body: put.body.slice(0, 200) });

        await page.waitForFunction(() => /changed on the server/i.test(document.body.innerText), { timeout: 20000, polling: 500 })
            .catch(() => {});
        await L.sleep(1000);
        out.steps.push({
            step: 'stale banner',
            banner: await page.evaluate(() => {
                const m = document.body.innerText.match(/This document was changed[^\n]*\n?[^\n]*/);
                return m ? m[0].replace(/\s+/g, ' ').trim() : null;
            }),
            keptDraft: (await L.readEditor(page)).includes(DRAFT),
        });
        await page.screenshot({ path: `${OUT}/stale-banner.png` });

        // The banner's Reload is the sanctioned way out - and it asks too, because there
        // is something to lose.
        await L.clickAriaButton(page, 'Refresh');
        await L.sleep(900);
        out.steps.push({ step: 'banner reload asks first', modal: await L.modal(page) });
        await L.clickButton(page, 'Yes', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(3000);
        out.steps.push({ step: 'after the confirmed reload', text: await L.readEditor(page), banner: await page.evaluate(() => /changed on the server/i.test(document.body.innerText)) });
        await page.screenshot({ path: `${OUT}/after-conflict-reload.png` });

        out.console = msgs.filter((m) => m.type !== 'log').slice(0, 20);
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
        if (original) {
            const restore = await L.api(host, t, 'PUT', `/meta/guests/${vmid}`, { mode: 'replace', text: original });
            out.restored = { status: restore.status, text: original };
        }
    }
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
