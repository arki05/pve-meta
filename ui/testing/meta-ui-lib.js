// Shared helpers for the pve-meta editor's headless checks (adapted from
// meta-ui-check.js / meta-ui-write-check.js).
const https = require('https');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function ticket(host, user, pass) {
    return new Promise((resolve, reject) => {
        const body = `username=${encodeURIComponent(user)}&password=${encodeURIComponent(pass)}`;
        const req = https.request(
            { host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
              headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': Buffer.byteLength(body) } },
            (r) => { let d = ''; r.on('data', (c) => (d += c)); r.on('end', () => { try { resolve(JSON.parse(d).data); } catch (e) { reject(new Error(d)); } }); },
        );
        req.on('error', reject); req.write(body); req.end();
    });
}

// A raw API call with the session's ticket + CSRF token, for changing a document from
// *outside* the page (that is what the version poll is supposed to notice).
function api(host, t, method, path, body) {
    return new Promise((resolve, reject) => {
        const payload = body === undefined ? null : JSON.stringify(body);
        const headers = { Cookie: `PVEAuthCookie=${encodeURIComponent(t.ticket)}` };
        if (payload !== null) {
            headers['Content-Type'] = 'application/json';
            headers['Content-Length'] = Buffer.byteLength(payload);
            headers.CSRFPreventionToken = t.CSRFPreventionToken;
        }
        const req = https.request({ host, port: 8006, path: `/api2/json${path}`, method, rejectUnauthorized: false, headers },
            (r) => { let d = ''; r.on('data', (c) => (d += c)); r.on('end', () => resolve({ status: r.statusCode, body: d })); });
        req.on('error', reject);
        if (payload !== null) req.write(payload);
        req.end();
    });
}

async function open(browser, host, t, query) {
    const page = await browser.newPage();
    await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
    await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
    await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
    return page;
}

// The text of the editor that is actually on screen (not the diff editor's models).
const readEditor = (page) => page.evaluate(() => {
    const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
    return e ? e.getValue() : null;
});

const selectorValue = (page) => page.evaluate(() => {
    const i = document.querySelector('.pwt-toolbar input');
    return i ? i.value : null;
});

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

async function clickAriaButton(page, label) {
    const ok = await page.evaluate((label) => {
        const b = Array.from(document.querySelectorAll('button, .pwt-button, [role="button"]'))
            .find((e) => e.getAttribute('aria-label') === label && !e.disabled && e.getAttribute('aria-disabled') !== 'true');
        if (!b) return false;
        b.click();
        return true;
    }, label);
    if (!ok) throw new Error('no enabled button with aria-label "' + label + '"');
}

// Open the "View as" picker and click one entry. No Escape afterwards: a confirmation
// dialog may be raised by the selection itself, and Escape would dismiss it.
async function pickView(page, label, settle) {
    await page.evaluate(() => { const i = document.querySelector('.pwt-toolbar input'); i.focus(); i.click(); });
    await sleep(900);
    const ok = await page.evaluate((label) => {
        const rows = document.querySelectorAll('.pwt-dialog.pwt-dropdown td, .pwt-dialog.pwt-dropdown [role="row"]');
        const row = Array.from(rows).find((e) => e.textContent.trim() === label);
        if (!row) return false;
        row.click();
        return true;
    }, label);
    if (!ok) throw new Error('no "' + label + '" entry in the View as picker');
    await sleep(settle === undefined ? 2500 : settle);
}

// Whatever modal is on top: its text and its buttons.
const modal = (page) => page.evaluate(() => {
    const dialogs = Array.from(document.querySelectorAll('.pwt-dialog:not(.pwt-dropdown)'));
    const d = dialogs[dialogs.length - 1];
    if (!d) return null;
    return {
        text: d.innerText.replace(/\s+/g, ' ').trim().slice(0, 200),
        buttons: Array.from(d.querySelectorAll('button, .pwt-button')).map((b) => b.textContent.trim()).filter(Boolean),
    };
});

const toolbarButtons = (page) => page.evaluate(() =>
    Array.from(document.querySelectorAll('.pwt-toolbar button, .pwt-toolbar .pwt-button')).map((b) => ({
        label: b.textContent.trim() || b.getAttribute('aria-label'),
        disabled: !!b.disabled || b.getAttribute('aria-disabled') === 'true',
    })));

const editorReadOnly = (page) => page.evaluate(() => {
    const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
    return e ? e.getOption(window.monaco.editor.EditorOption.readOnly) : null;
});

// Type into the model, the same path a keystroke takes (onDidChangeModelContent -> the
// glue -> Msg::EditorInput). Works even when the editor is read-only, which is exactly
// what makes the read-only assertions about the *editor*, not about the model.
const editModel = (page, from, to) => page.evaluate((from, to) => {
    const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
    const model = e.getModel();
    model.setValue(model.getValue().replace(from, to));
}, from, to);

module.exports = { sleep, ticket, api, open, readEditor, selectorValue, clickButton, clickAriaButton, pickView, modal, toolbarButtons, editorReadOnly, editModel };
