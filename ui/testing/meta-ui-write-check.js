// meta-ui-write-check.js - exercises the pve-meta editor's full round trip:
// load -> switch view -> edit -> Apply (diff dialog) -> PUT -> reload.
//
// Usage: node meta-ui-write-check.js <host> <vmid> <newHost>
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const oldHost = process.argv[4] || 'ct200.example';
const newHost = process.argv[5] || 'ct200-edited.example';
const OUT = '/root/headless/shots';

function ticket() {
    return new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            { host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
              headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length } },
            (r) => { let d = ''; r.on('data', (c) => (d += c)); r.on('end', () => { try { resolve(JSON.parse(d).data); } catch (e) { reject(new Error(d)); } }); },
        );
        req.on('error', reject); req.write(body); req.end();
    });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Plain DOM clicks throughout: a leftover combobox popover would swallow a real mouse
// click and make the run flaky.
async function clickButton(page, label, root) {
    const ok = await page.evaluate((label, root) => {
        const scope = root ? document.querySelector(root) : document;
        if (!scope) return false;
        const b = Array.from(scope.querySelectorAll('button, .pwt-button, [role="button"]'))
            .find((e) => e.textContent.trim() === label && !e.disabled && e.getAttribute('aria-disabled') !== 'true');
        if (!b) return false;
        b.click();
        return true;
    }, label, root || null);
    if (!ok) throw new Error('no enabled button labelled "' + label + '"' + (root ? ' in ' + root : ''));
}

async function pickView(page, label) {
    await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
    await sleep(900);
    const ok = await page.evaluate((label) => {
        const rows = document.querySelectorAll('.pwt-dialog.pwt-dropdown td, .pwt-dialog.pwt-dropdown [role="row"]');
        const row = Array.from(rows).find((e) => e.textContent.trim() === label);
        if (!row) return false;
        row.click();
        return true;
    }, label);
    await page.keyboard.press('Escape');
    await sleep(2500);
    if (!ok) throw new Error('no "' + label + '" entry in the View as picker');
}

async function main() {
    const t = await ticket();
    const out = { steps: [] };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    try {
        const page = await browser.newPage();
        const msgs = [];
        page.on('console', (m) => msgs.push({ type: m.type(), text: m.text() }));
        page.on('pageerror', (e) => msgs.push({ type: 'pageerror', text: e.message }));
        const writes = [];
        page.on('response', (r) => { if (/api2\/json\/meta/.test(r.url()) && r.request().method() !== 'GET') writes.push({ method: r.request().method(), url: r.url(), status: r.status() }); });

        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
        await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);

        await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}&type=lxc&node=pvemeta-node1&theme=light`,
            { waitUntil: 'networkidle2', timeout: 60000 });
        await page.waitForFunction(() => document.querySelector('.monaco-editor'), { timeout: 30000, polling: 300 });
        await sleep(1500);

        const readEditor = () => page.evaluate(() => window.monaco.editor.getModels()[0].getValue());
        out.steps.push({ step: 'initial load', text: await readEditor() });

        // --- switch the view to `traefik`: the prefix must be stripped ----------
        await pickView(page, 'traefik');
        out.steps.push({ step: 'view=traefik', text: await readEditor() });
        await page.screenshot({ path: `${OUT}/view-traefik.png` });

        // --- back to the whole document, then edit it --------------------------
        await pickView(page, 'Whole document');
        out.steps.push({ step: 'view=whole document', text: await readEditor() });

        // Edit through Monaco's own model, so the change travels the same path a
        // keystroke would (onDidChangeModelContent -> the glue -> Msg::EditorInput).
        await page.evaluate((oldHost, newHost) => {
            const model = window.monaco.editor.getModels()[0];
            model.setValue(model.getValue().replace(oldHost, newHost));
        }, oldHost, newHost);
        await sleep(800);
        out.steps.push({ step: 'edited', text: await readEditor() });

        // --- Apply -> diff dialog ---------------------------------------------
        await clickButton(page, 'Apply');
        await page.waitForFunction(() => document.querySelectorAll('.monaco-diff-editor').length > 0, { timeout: 20000, polling: 300 });
        await sleep(1500);
        out.steps.push({ step: 'diff dialog', ...(await page.evaluate(() => ({
            dialogTitle: (document.querySelector('.pwt-dialog:not(.pwt-dropdown) .pwt-panel-header-text') || {}).textContent,
            insertedLines: document.querySelectorAll('.line-insert').length,
            deletedLines: document.querySelectorAll('.line-delete').length,
        }))) });
        await page.screenshot({ path: `${OUT}/diff-dialog.png` });

        // --- confirm ----------------------------------------------------------
        await clickButton(page, 'Apply', '.pwt-dialog:not(.pwt-dropdown)');
        await sleep(4000);
        out.steps.push({ step: 'after apply', text: await readEditor(), writes });
        await page.screenshot({ path: `${OUT}/after-apply.png` });

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
