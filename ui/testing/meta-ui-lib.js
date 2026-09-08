// Shared helpers for the pve-meta tree page's headless checks.
//
// The page is a pwt `DataTable` over a `TreeStore`, so everything here reaches it the way
// a user does: through the rendered grid (`[role="row"]` / `[role="gridcell"]`), the
// toolbar buttons and the dialogs. Nothing pokes at Yew or at the store.
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

const doc = (host, t, vmid) => api(host, t, 'GET', `/meta/guests/${vmid}?format=json`)
    .then((r) => JSON.parse(r.body).data);

// `operators` is an argument, not a fixture: `GET /meta/operators` is revision 5 and the
// lab node still answers 501, so a check that needs a grammar installs one by answering
// that one request in the page itself. Everything else goes to the real API.
async function open(browser, host, t, query, operators) {
    const page = await browser.newPage();
    await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
    await page.setCookie({ name: 'PVEThemeCookie', value: 'crisp', domain: host, path: '/', secure: true });
    await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
    if (operators) {
        await page.evaluateOnNewDocument((ops) => {
            const real = window.fetch.bind(window);
            window.fetch = function (input, init) {
                const url = typeof input === 'string' ? input : (input && input.url) || '';
                if (url.indexOf('/meta/operators') !== -1) {
                    return Promise.resolve(new Response(JSON.stringify({ data: ops }), {
                        status: 200, headers: { 'Content-Type': 'application/json' },
                    }));
                }
                return real(input, init);
            };
        }, operators);
    }
    return page;
}

// Every row of the tree as {key, value, owner, level, description, expanded}.
const rows = (page) => page.evaluate(() => {
    const table = document.querySelector('.pwt-datatable, [role="grid"], table');
    if (!table) return [];
    return Array.from(table.querySelectorAll('[role="row"]'))
        .filter((r) => r.querySelector('[role="gridcell"], td'))
        .map((r) => {
            const cells = Array.from(r.querySelectorAll('[role="gridcell"], td'));
            const text = (i) => (cells[i] ? cells[i].innerText.replace(/\s+/g, ' ').trim() : '');
            const keyCell = cells[0];
            const lines = keyCell ? keyCell.innerText.split('\n').map((s) => s.trim()).filter(Boolean) : [];
            return {
                key: lines[0] || '',
                description: lines.slice(1).join(' '),
                value: text(1),
                owner: text(2),
                selected: r.getAttribute('aria-selected') === 'true',
                expanded: !!keyCell && !!keyCell.querySelector('.fa-caret-down'),
                collapsed: !!keyCell && !!keyCell.querySelector('.fa-caret-right'),
                dimmed: !!keyCell && !!keyCell.querySelector('.pwt-opacity-50'),
            };
        });
});

async function clickRow(page, key) {
    const ok = await page.evaluate((key) => {
        const rows = Array.from(document.querySelectorAll('[role="row"]'));
        const row = rows.find((r) => {
            const cell = r.querySelector('[role="gridcell"], td');
            return cell && cell.innerText.split('\n')[0].trim() === key;
        });
        if (!row) return false;
        const cell = row.querySelector('[role="gridcell"], td');
        cell.click();
        return true;
    }, key);
    if (!ok) throw new Error('no row with key "' + key + '"');
    await sleep(300);
}

// Plain DOM clicks throughout: a leftover popover would swallow a real mouse click and
// make the run flaky.
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
    await sleep(400);
}

// The topmost dialog: its text and its buttons.
const modal = (page) => page.evaluate(() => {
    const dialogs = Array.from(document.querySelectorAll('.pwt-dialog:not(.pwt-dropdown)'));
    const d = dialogs[dialogs.length - 1];
    if (!d) return null;
    return {
        title: (d.querySelector('.pwt-dialog-title, header') || {}).innerText || '',
        text: d.innerText.replace(/\s+/g, ' ').trim().slice(0, 300),
        buttons: Array.from(d.querySelectorAll('button, .pwt-button')).map((b) => b.textContent.trim()).filter(Boolean),
        // pwt fields are driven by the FormContext and carry no DOM `name`; they are
        // labelled with aria-labelledby, which is what a screen reader (and this
        // harness) has to go by.
        fields: Array.from(d.querySelectorAll('input, textarea')).map((i) => {
            const label = i.getAttribute('aria-labelledby');
            const el = label && document.getElementById(label);
            return { name: el ? el.innerText.trim() : '', value: i.value, type: i.type };
        }),
    };
});

// Type into a labelled field of the open dialog, the way a keystroke would.
async function setField(page, name, value) {
    const ok = await page.evaluate((name, value) => {
        const dialogs = Array.from(document.querySelectorAll('.pwt-dialog:not(.pwt-dropdown)'));
        const d = dialogs[dialogs.length - 1];
        const el = d && Array.from(d.querySelectorAll('input, textarea')).find((i) => {
            const id = i.getAttribute('aria-labelledby');
            const label = id && document.getElementById(id);
            return label && label.innerText.trim() === name;
        });
        if (!el) return false;
        const proto = el instanceof window.HTMLTextAreaElement
            ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
        if (el.type === 'checkbox') {
            if (el.checked !== !!value) el.click();
            return true;
        }
        Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return true;
    }, name, value);
    if (!ok) throw new Error('no field named "' + name + '" in the open dialog');
    await sleep(300);
}

// A pwt Combobox is a picker, not a text field: open it and click the entry.
async function pickCombo(page, name, item) {
    const opened = await page.evaluate((name) => {
        const dialogs = Array.from(document.querySelectorAll('.pwt-dialog:not(.pwt-dropdown)'));
        const d = dialogs[dialogs.length - 1];
        const el = d && Array.from(d.querySelectorAll('input')).find((i) => {
            const id = i.getAttribute('aria-labelledby');
            const label = id && document.getElementById(id);
            return label && label.innerText.trim() === name;
        });
        if (!el) return false;
        el.focus();
        el.click();
        return true;
    }, name);
    if (!opened) throw new Error('no combobox labelled "' + name + '"');
    await sleep(700);
    const picked = await page.evaluate((item) => {
        const rows = document.querySelectorAll('.pwt-dialog.pwt-dropdown td, .pwt-dialog.pwt-dropdown [role="row"]');
        const row = Array.from(rows).find((e) => e.textContent.trim() === item);
        if (!row) return false;
        row.click();
        return true;
    }, item);
    if (!picked) throw new Error('no "' + item + '" entry in the ' + name + ' picker');
    await sleep(400);
}

const toolbarButtons = (page) => page.evaluate(() =>
    Array.from(document.querySelectorAll('.pwt-toolbar button, .pwt-toolbar .pwt-button')).map((b) => ({
        label: b.textContent.trim() || b.getAttribute('aria-label'),
        disabled: !!b.disabled || b.getAttribute('aria-disabled') === 'true',
    })));

// The text of the Monaco editor that is on screen (not the diff editor's models).
const readEditor = (page) => page.evaluate(() => {
    if (!window.monaco) return null;
    const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
    return e ? e.getValue() : null;
});

const editModel = (page, from, to) => page.evaluate((from, to) => {
    const e = window.monaco.editor.getEditors().find((e) => !e.getOriginalEditor);
    const model = e.getModel();
    model.setValue(model.getValue().replace(from, to));
}, from, to);

const url = (host, query) => `https://${host}:8006/pve2/js/pve-meta-ui/index.html?${query}`;

async function waitForTree(page, timeout) {
    const deadline = Date.now() + (timeout || 15000);
    for (;;) {
        const n = await page.evaluate(() => document.querySelectorAll('[role="row"]').length);
        if (n > 1) return;
        if (Date.now() > deadline) throw new Error('the tree never rendered a row');
        await sleep(250);
    }
}

module.exports = {
    sleep, ticket, api, doc, open, rows, clickRow, clickButton, modal, setField, pickCombo,
    toolbarButtons, readEditor, editModel, url, waitForTree,
};
