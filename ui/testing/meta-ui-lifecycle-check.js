// meta-ui-lifecycle-check.js - Monaco instances are disposed when the page stops showing
// what they were mounted for (view switch, dialog close), and the theme switch reaches
// the editor.
//
// Usage: node meta-ui-lifecycle-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const OUT = '/root/headless/shots';

const counts = (page) => page.evaluate(() => ({
    editors: window.monaco.editor.getEditors().length,
    diffEditors: window.monaco.editor.getDiffEditors().length,
    models: window.monaco.editor.getModels().length,
}));

const background = (page) => page.evaluate(() => {
    const el = document.querySelector('.monaco-editor .monaco-editor-background') || document.querySelector('.monaco-editor');
    return el ? window.getComputedStyle(el).backgroundColor : null;
});

async function main() {
    const t = await L.ticket(host, 'root@pam', 'pvelab');
    const out = { steps: [] };
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

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);
        out.steps.push({ step: 'loaded', ...(await counts(page)) });

        // --- view switches must not accumulate editors or models ---------------
        for (const view of ['traefik', 'Whole document', 'netbird', 'Whole document']) {
            await L.pickView(page, view, 2500);
            out.steps.push({ step: `after switching to ${view}`, selector: await L.selectorValue(page), ...(await counts(page)) });
        }

        // --- the diff editor lives exactly as long as its dialog ---------------
        await page.evaluate(() => {
            const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
            const model = e.getModel();
            model.setValue(model.getValue() + 'draft__: touched\n');
        });
        await L.sleep(700);
        await L.clickButton(page, 'Apply');
        await page.waitForFunction(() => document.querySelectorAll('.monaco-diff-editor').length > 0, { timeout: 20000, polling: 300 });
        await L.sleep(1200);
        out.steps.push({ step: 'diff dialog open', ...(await counts(page)) });

        await L.clickButton(page, 'Cancel', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(1500);
        out.steps.push({ step: 'diff dialog closed', ...(await counts(page)), diffNodes: await page.evaluate(() => document.querySelectorAll('.monaco-diff-editor').length) });

        // Open it once more and leave the view instead: the dialog and its editor must go.
        await L.clickButton(page, 'Apply');
        await page.waitForFunction(() => document.querySelectorAll('.monaco-diff-editor').length > 0, { timeout: 20000, polling: 300 });
        await L.sleep(1000);
        await L.clickButton(page, 'Discard');
        await L.sleep(500);
        await L.clickButton(page, 'Yes', '.pwt-dialog:not(.pwt-dropdown)').catch(() => {});
        await L.sleep(1500);
        out.steps.push({ step: 'after discarding with the dialog open', ...(await counts(page)) });

        // --- theme -------------------------------------------------------------
        out.steps.push({ step: 'light theme', background: await background(page), darkClass: await page.evaluate(() => document.documentElement.classList.contains('pwt-dark-mode')) });
        await page.screenshot({ path: `${OUT}/standalone-light.png` });

        await page.evaluate(() => {
            localStorage.setItem('ThemeMode', 'dark');
            document.dispatchEvent(new Event('pwt-theme-changed'));
        });
        await L.sleep(2500);
        out.steps.push({ step: 'after switching to dark', background: await background(page), darkClass: await page.evaluate(() => document.documentElement.classList.contains('pwt-dark-mode')) });
        await page.screenshot({ path: `${OUT}/standalone-dark.png` });

        await page.evaluate(() => {
            localStorage.setItem('ThemeMode', 'light');
            document.dispatchEvent(new Event('pwt-theme-changed'));
        });
        await L.sleep(2500);
        out.steps.push({ step: 'back to light', background: await background(page), darkClass: await page.evaluate(() => document.documentElement.classList.contains('pwt-dark-mode')) });

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
