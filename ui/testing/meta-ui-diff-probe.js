// Is the diff editor that monaco.editor.getDiffEditors() still lists after the dialog
// closed a live widget, or a dead entry in monaco's own registry?
const puppeteer = require('puppeteer-core');
const L = require('./meta-ui-lib');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';

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
        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await L.sleep(2000);

        await page.evaluate(() => {
            const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
            e.getModel().setValue(e.getModel().getValue() + 'draft__: touched\n');
        });
        await L.sleep(700);
        await L.clickButton(page, 'Apply');
        await page.waitForFunction(() => document.querySelectorAll('.monaco-diff-editor').length > 0, { timeout: 20000, polling: 300 });
        await L.sleep(1200);
        out.open = await page.evaluate(() => window.monaco.editor.getDiffEditors().map((d) => ({
            hasModel: !!d.getModel(),
            inDom: !!(d.getContainerDomNode && d.getContainerDomNode() && d.getContainerDomNode().isConnected),
        })));

        await L.clickButton(page, 'Cancel', '.pwt-dialog:not(.pwt-dropdown)');
        await L.sleep(1500);
        out.closed = await page.evaluate(() => window.monaco.editor.getDiffEditors().map((d) => {
            let hasModel = null, inDom = null, error = null;
            try { hasModel = !!d.getModel(); } catch (e) { error = String(e); }
            try { const n = d.getContainerDomNode ? d.getContainerDomNode() : null; inDom = !!(n && n.isConnected); } catch (e) { error = String(e); }
            return { hasModel, inDom, error };
        }));
        out.models = await page.evaluate(() => window.monaco.editor.getModels().length);
        out.diffNodes = await page.evaluate(() => document.querySelectorAll('.monaco-diff-editor').length);
        await page.close();
    } catch (e) {
        out.error = String(e);
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(out, null, 2));
}

main().catch((e) => { console.error('FATAL', e); process.exit(1); });
