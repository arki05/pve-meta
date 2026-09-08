// meta-ui-reload-invalidate-check.js - live confirmation of P8's fix.
//
// Dirty the editor, fire Apply, hold its PUT response back with request interception,
// then confirm a Reload (the ConfirmButton path, Msg::Reload) while the Apply is still
// in flight. Before the fix, the late Apply answer (a 409, since the reload's own load
// moves the digest) would land on the freshly reloaded, non-dirty document and show a
// stale-conflict banner with a no-op Reload button. After the fix, `invalidate()` should
// make the late answer's `requests.accepts()` fail, so it is dropped with a console
// warning and no banner appears.
//
// Usage: node meta-ui-reload-invalidate-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';
const delayMs = 6000;

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
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text().slice(0, 300) }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));

        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const url = req.url();
            if (req.method() === 'PUT' && /api2\/json\/meta\/guests/.test(url)) {
                setTimeout(() => req.continue().catch(() => {}), delayMs);
            } else {
                req.continue().catch(() => {});
            }
        });

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        out.original = await L.readEditor(page);

        // Dirty the buffer.
        await L.editModel(page, 'ct200.example', 'ct200.example-EDIT');
        await L.sleep(300);

        out.toolbarBefore = await L.toolbarButtons(page);
        out.dirtyEditor = await L.readEditor(page);

        // Fire Apply: open the diff dialog, click Apply. The PUT is held for `delayMs`.
        await L.clickButton(page, 'Apply');
        await L.sleep(800);
        out.modalAfterApplyClick = await L.modal(page);
        await page.evaluate(() => {
            const dialogs = Array.from(document.querySelectorAll('.pwt-dialog:not(.pwt-dropdown)'));
            const d = dialogs[dialogs.length - 1];
            const b = d && Array.from(d.querySelectorAll('button, .pwt-button')).find((e) => e.textContent.trim() === 'Apply');
            if (b) b.click(); else throw new Error('no Apply button in the diff dialog');
        });
        await L.sleep(300);

        // Confirm a Reload while the Apply PUT is still held back.
        // The dirty Reload button is a ConfirmButton; click it, then confirm the dialog.
        await page.evaluate(() => {
            const b = Array.from(document.querySelectorAll('button, .pwt-button'))
                .find((e) => e.getAttribute('aria-label') === 'Refresh');
            if (b) b.click();
        });
        await L.sleep(500);
        out.confirmModal = await L.modal(page);
        await page.evaluate(() => {
            const dialogs = Array.from(document.querySelectorAll('.pwt-dialog:not(.pwt-dropdown)'));
            const d = dialogs[dialogs.length - 1];
            const b = d && Array.from(d.querySelectorAll('button, .pwt-button')).find((e) => e.textContent.trim() === 'Yes');
            if (b) b.click(); else throw new Error('no Yes button in the confirm dialog');
        });
        await L.sleep(1500);
        out.afterReloadConfirm = { editor: await L.readEditor(page), body: (await page.evaluate(() => document.body.innerText)).slice(0, 300) };

        // Let the held Apply PUT answer land.
        await L.sleep(delayMs + 3000);
        out.final = { editor: await L.readEditor(page), body: (await page.evaluate(() => document.body.innerText)).slice(0, 400) };
        await page.screenshot({ path: `${OUT}/reload-invalidate-final.png` });

        out.staleBannerPresent = /changed on the server/i.test(out.final.body);
        out.verdict = out.staleBannerPresent
            ? 'NOT FIXED: the late Apply answer (after Reload) produced a stale/conflict banner'
            : 'clean: the late Apply answer after Reload was dropped, no banner';

        out.console = msgs.filter((m) => m.type !== 'log').slice(0, 30);
        await page.close();

        // The held Apply PUT lands on the server regardless of the verdict above (the
        // interception above only delays *sending* the request, it never drops it, and
        // the digest it carries still matches -- the Reload in between was a plain GET).
        // So CT 200's on-disk document is now the -EDIT text this check wrote. Undo that
        // with a clean, uninstrumented page (no request interception, fresh ticket/digest)
        // so the check is idempotent and never leaves the fixture dirtied.
        out.restore = { attempted: false };
        try {
            const restorePage = await L.open(browser, host, t);
            await restorePage.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
                { waitUntil: 'networkidle2', timeout: 60000 });
            await restorePage.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
            await L.sleep(2000);

            const current = await L.readEditor(restorePage);
            out.restore.beforeRestore = current;
            if (current === out.original) {
                out.restore.attempted = false;
                out.restore.note = 'document already matched the original; no write needed';
            } else {
                out.restore.attempted = true;
                await restorePage.evaluate((original) => {
                    const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
                    e.getModel().setValue(original);
                }, out.original);
                await L.sleep(300);
                await L.clickButton(restorePage, 'Apply');
                await restorePage.waitForFunction(() => document.querySelectorAll('.monaco-diff-editor').length > 0, { timeout: 20000, polling: 300 });
                await L.sleep(800);
                await L.clickButton(restorePage, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
                await L.sleep(2000);
                out.restore.afterRestore = await L.readEditor(restorePage);
                out.restore.ok = out.restore.afterRestore === out.original;
            }
            await restorePage.close();
        } catch (e) {
            out.restore.error = String(e);
        }
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }

    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
