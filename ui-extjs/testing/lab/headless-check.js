// headless-check.js — headless verification of PVE.meta.TreePanel against the real
// pve-manager SPA on a lab host: open the tab, add a key, edit it, remove it, apply
// from the Text card, provoke a 409, then the datacenter tab's registry grid.
// Usage: node headless-check.js <host> <vmid> [light|dark] [--ro]
// --ro stubs GET /meta/access with full read and no write, so the "Read-only" toolbar
//   label can be seen without a second lab principal's credentials; implies read-only
//   (every step that writes is skipped).
// Needs puppeteer-core and a chromium binary; writes screenshots to /root/headless/shots.
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';
const theme = process.argv[4] === 'dark' ? 'dark' : 'light';
const readOnly = process.argv.includes('--ro');
const ACCESS_STUB = { read: 1, write: 0 };
const out = '/root/headless/shots';

function api(path, method, ticket, csrf, body) {
    return new Promise((resolve, reject) => {
        const data = body || '';
        const req = https.request(
            {
                host,
                port: 8006,
                path,
                method,
                rejectUnauthorized: false,
                headers: {
                    'Content-Type': 'application/x-www-form-urlencoded',
                    'Content-Length': Buffer.byteLength(data),
                    Cookie: ticket ? 'PVEAuthCookie=' + encodeURIComponent(ticket) : '',
                    CSRFPreventionToken: csrf || '',
                },
            },
            (r) => {
                let d = '';
                r.on('data', (c) => (d += c));
                r.on('end', () => resolve(JSON.parse(d)));
            },
        );
        req.on('error', reject);
        req.write(data);
        req.end();
    });
}

async function ticket() {
    const t = await api('/api2/json/access/ticket', 'POST', null, null, 'username=root%40pam&password=pvelab');
    return { ticket: t.data.ticket, csrf: t.data.CSRFPreventionToken };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A fresh, logged-in page, retrying while pve-manager's SPA races GuiCap on the
// first paint (a known flake) -- bounded, not indefinite.
async function openPage(browser, tk, csrf, result) {
    for (let attempt = 1; attempt <= 5; attempt++) {
        const page = await browser.newPage();
        page.on('console', (m) => m.type() === 'error' && result.console.push({ type: 'error', text: m.text() }));
        page.on('pageerror', (e) => result.console.push({ type: 'pageerror', text: e.message }));
        page.on('requestfailed', (r) =>
            result.console.push({ type: 'requestfailed', text: r.url() + ' ' + (r.failure() || {}).errorText }),
        );
        await page.setCookie(
            { name: 'PVEAuthCookie', value: tk, domain: host, path: '/', secure: true },
            {
                name: 'PVEThemeCookie',
                value: theme === 'dark' ? 'proxmox-dark' : 'crisp',
                domain: host,
                path: '/',
                secure: true,
            },
        );
        await page.evaluateOnNewDocument((c) => {
            try {
                sessionStorage.setItem('CSRFPreventionToken', c);
            } catch (_e) {}
        }, csrf);
        if (readOnly) {
            await page.setRequestInterception(true);
            page.on('request', (req) => {
                if (/\/api2\/(extjs|json)\/meta\/access/.test(req.url())) {
                    req.respond({
                        status: 200,
                        contentType: 'application/json',
                        body: JSON.stringify({ success: 1, data: ACCESS_STUB }),
                    });
                    return;
                }
                req.continue();
            });
        }
        await page.goto(`https://${host}:8006/`, { waitUntil: 'networkidle2', timeout: 60000 });
        const ok = await page
            .waitForFunction(() => document.querySelectorAll('.x-treelist-item-text').length > 0, {
                timeout: 15000,
                polling: 200,
            })
            .then(() => true)
            .catch(() => false);
        if (ok) {
            await sleep(1200);
            return page;
        }
        await page.close();
        if (attempt === 5) {
            throw new Error('SPA never rendered');
        }
    }
    return undefined;
}

// Expands the resource tree down to guest `id`, or clicks the Datacenter root
// (`kind === 'dc'`), then opens the Metadata nav item pve-ext's loader added.
async function openTab(page, kind, id) {
    if (kind === 'dc') {
        const h = await page.evaluateHandle(() => {
            const tree = Ext.ComponentQuery.query('treepanel')[0];
            const rec = tree.getStore().getNodeById('root');
            return rec ? tree.getView().getNode(rec) : null;
        });
        if (!h.asElement()) {
            throw new Error('no datacenter node in the resource tree');
        }
        await h.asElement().click();
    } else {
        const h = await page.evaluateHandle((v) => {
            return new Promise((resolve, reject) => {
                const tree = Ext.ComponentQuery.query('treepanel')[0];
                const rec =
                    tree.getStore().getNodeById('lxc/' + v) || tree.getStore().getNodeById('qemu/' + v);
                if (!rec) {
                    reject(new Error('no guest ' + v + ' in the resource tree'));
                    return;
                }
                const anc = [];
                let p = rec.parentNode;
                while (p) {
                    anc.unshift(p);
                    p = p.parentNode;
                }
                let i = 0;
                (function next() {
                    if (i >= anc.length) {
                        setTimeout(() => resolve(tree.getView().getNode(rec)), 300);
                        return;
                    }
                    const n = anc[i++];
                    n.isExpanded() ? next() : n.expand(false, next);
                })();
            });
        }, id);
        await h.asElement().click();
    }
    await sleep(1500);
    const nav = await page.evaluateHandle(() =>
        Array.from(document.querySelectorAll('.x-treelist-item-text')).find(
            (e) => e.textContent.trim() === 'Metadata',
        ),
    );
    if (!nav.asElement()) {
        throw new Error('no Metadata nav item for ' + kind);
    }
    await nav.asElement().click();
    await sleep(5000);
}

const rows = (page) =>
    page.evaluate(() => {
        const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
        if (!p) return null;
        const r = [];
        p.getRootNode().cascadeBy((n) => n.data.path && r.push(n.data.path + '=' + n.data.valueText));
        return r;
    });

async function main() {
    const { ticket: tk, csrf } = await ticket();
    const result = { theme, readOnly, console: [], checks: {} };
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1500,950'],
        defaultViewport: { width: 1500, height: 950 },
    });
    try {
        const page = await openPage(browser, tk, csrf, result);

        await openTab(page, 'lxc', vmid);
        result.checks.rowsBefore = await rows(page);
        result.checks.accessLabel = await page.evaluate(() => {
            const label = Ext.ComponentQuery.query('pveMetaTreePanel')[0].down('#accessText');
            return label.isVisible() ? label.el.dom.textContent.trim() : null;
        });
        await page.screenshot({ path: `${out}/extjs-tree-${theme}.png` });

        if (readOnly) {
            console.log(JSON.stringify(result, null, 2));
            return;
        }

        // --- Add, through the Add Key window --------------------------------
        // The window's Add is the write: `PUT ?view=added.by.ui&mode=replace`, and
        // `view::replace` creates `added` and `added.by` on the way.
        await page.evaluate(() => Ext.ComponentQuery.query('pveMetaTreePanel')[0].addKey(''));
        await sleep(800);
        await page.evaluate(() => {
            const w = Ext.ComponentQuery.query('pveMetaAddKeyWindow')[0];
            w.down('[name=key]').setValue('added.by.ui');
            w.down('[name=kind]').setValue('number');
            w.down('[name=value]').setValue('7');
        });
        await page.screenshot({ path: `${out}/extjs-addkey.png` });
        await page.evaluate(() => Ext.ComponentQuery.query('pveMetaAddKeyWindow')[0].submit());
        await sleep(3000);
        result.checks.afterAdd = (await rows(page)).filter((r) => r.startsWith('added'));

        // --- Edit that value, through the row editor -------------------------
        await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            let n = null;
            p.getRootNode().cascadeBy((x) => {
                if (x.data.path === 'added.by.ui') n = x;
            });
            if (n) p.editRow(n);
        });
        await sleep(800);
        result.checks.rowEditor = await page.evaluate(() => {
            const w = Ext.ComponentQuery.query('pveMetaEditValueWindow')[0];
            if (!w) return 'no row editor';
            w.down('#valueField').setValue(9);
            w.submit();
            return 'submitted';
        });
        await sleep(3000);
        result.checks.afterEdit = (await rows(page)).filter((r) => r.startsWith('added'));

        // --- Remove: `DELETE ?view=added&digest=` ----------------------------
        result.checks.remove = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            let n = null;
            p.getRootNode().cascadeBy((x) => {
                if (x.data.path === 'added') n = x;
            });
            if (!n) return 'row not found';
            p.removeKey(n);
            return 'removed';
        });
        await sleep(3000);
        result.checks.afterRemove = (await rows(page)).filter((r) => r.startsWith('added'));

        // --- The Text card's Apply sends the buffer --------------------------
        // The one write that is text rather than a subtree: a `#` comment is not
        // part of the document model, so it survives only because this path sends
        // exactly what was typed.
        await page.evaluate(() => {
            Ext.ComponentQuery.query('pveMetaTreePanel')[0].down('#modeBtn').setValue('text');
        });
        await sleep(9000);
        result.checks.textApply = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            if (!p.textEditor) return 'no editor';
            p.textEditor.setValue(
                '# written from the text card\n' + p.textEditor.getValue() + 'applied_from_text: yes\n',
            );
            p.applyText();
            return 'applied';
        });
        await sleep(4000);
        await page.screenshot({ path: `${out}/extjs-text-apply-${theme}.png` });
        result.checks.afterTextApply = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            return {
                comment: p.textOriginal.indexOf('# written from the text card') === 0,
                key: p.textOriginal.indexOf('applied_from_text') !== -1,
            };
        });
        // Back to the tree: the buffer is clean, so nothing is asked.
        await page.evaluate(() => {
            Ext.ComponentQuery.query('pveMetaTreePanel')[0].down('#modeBtn').setValue('tree');
        });
        await sleep(3000);
        result.checks.backInTree = await page.evaluate(
            () => Ext.ComponentQuery.query('pveMetaTreePanel')[0].mode,
        );
        result.checks.monacoDisposed = await page.evaluate(() => window.monaco.editor.getModels().length);
        await api(`/api2/json/meta/guests/${vmid}?view=applied_from_text`, 'DELETE', tk, csrf, '');

        // --- A concurrent write is a 409: one message, and a reload ----------
        // There is no background poll: the digest on every write is what catches a
        // change made elsewhere, and the next write is when you find out.
        await api(
            `/api2/json/meta/guests/${vmid}`,
            'PUT',
            tk,
            csrf,
            'view=reload_probe&mode=replace&data=' + encodeURIComponent('"set from outside"'),
        );
        await sleep(500);
        // The panel still holds the digest from before that outside write.
        await page.evaluate(() =>
            Ext.ComponentQuery.query('pveMetaTreePanel')[0].sendEdit({
                path: 'conflict_probe',
                op: 'set',
                value: 'from the ui',
            }),
        );
        await sleep(3000);
        result.checks.conflict = await page.evaluate(() => {
            const b = Ext.ComponentQuery.query('messagebox').find((m) => m.isVisible());
            const title = b ? b.title : null;
            if (b) b.close();
            return { title: title };
        });
        await sleep(3000);
        await page.screenshot({ path: `${out}/extjs-conflict-${theme}.png` });
        // The 409 reloaded, so the outside write is on screen and ours is not.
        result.checks.afterConflict = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            const r = [];
            p.getRootNode().cascadeBy((n) => n.data.path && r.push(n.data.path));
            return {
                outsideWrite: r.indexOf('reload_probe') !== -1,
                ourWrite: r.indexOf('conflict_probe') !== -1,
            };
        });
        await api(`/api2/json/meta/guests/${vmid}?view=reload_probe`, 'DELETE', tk, csrf, '');

        // --- The datacenter tab: the prefix registry list, and no document ---
        await openTab(page, 'dc');
        result.checks.datacenter = await page.evaluate(() => {
            const grids = Ext.ComponentQuery.query('pveMetaRegistryGrid');
            return {
                grids: grids.length,
                treePanels: Ext.ComponentQuery.query('pveMetaTreePanel').length,
                rows: grids.map((g) => g.getStore().getCount()),
            };
        });
        await page.screenshot({ path: `${out}/extjs-datacenter.png` });

        // A pve-manager flake unrelated to this panel.
        result.console = result.console.filter((m) => !/Mapping\.Audit/.test(m.text));
    } catch (e) {
        result.error = e.message;
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(result, null, 2));
}

main().catch((e) => {
    console.error('FAILED: ' + e.stack);
    process.exit(1);
});
