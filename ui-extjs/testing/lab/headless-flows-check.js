// headless-flows-check.js — the flows headless-tab-check.js does not cover: Add, the
// row editor, Remove, the Text card's Apply, a 409, and the datacenter document.
// Each of the first four is one write with the digest, followed by a reload.
// Usage: node headless-flows-check.js <host> <vmid>
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '201';
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function openTab(page, kind, id) {
    if (kind === 'dc') {
        const h = await page.evaluateHandle(() => {
            const tree = Ext.ComponentQuery.query('treepanel')[0];
            const rec = tree.getStore().getNodeById('root');
            return rec ? tree.getView().getNode(rec) : null;
        });
        if (!h.asElement()) throw new Error('no datacenter node in the resource tree');
        await h.asElement().click();
    } else {
        const h = await page.evaluateHandle((v) => {
            return new Promise((resolve, reject) => {
                const tree = Ext.ComponentQuery.query('treepanel')[0];
                const rec = tree.getStore().getNodeById('lxc/' + v);
                if (!rec) return reject(new Error('no lxc/' + v));
                const anc = [];
                let p = rec.parentNode;
                while (p) {
                    anc.unshift(p);
                    p = p.parentNode;
                }
                let i = 0;
                (function next() {
                    if (i >= anc.length) return setTimeout(() => resolve(tree.getView().getNode(rec)), 300);
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
    if (!nav.asElement()) throw new Error('no Metadata nav item for ' + kind);
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
    const t = await api('/api2/json/access/ticket', 'POST', null, null, 'username=root%40pam&password=pvelab');
    const tk = t.data.ticket;
    const csrf = t.data.CSRFPreventionToken;
    const result = { console: [], checks: {} };

    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1500,950'],
        defaultViewport: { width: 1500, height: 950 },
    });
    try {
        // The initial Datacenter view can race with GuiCap and leave the nav treelist
        // empty - a known pve-manager flake. Retry with a fresh page, bounded.
        let page;
        for (let attempt = 1; attempt <= 5; attempt++) {
            page = await browser.newPage();
            page.on('pageerror', (e) =>
                result.console.push({ type: 'pageerror', text: e.message, stack: String(e.stack).split('\n').slice(0, 6) }),
            );
            page.on(
                'console',
                (m) => m.type() === 'error' && result.console.push({ type: 'error', text: m.text() }),
            );
            page.on('requestfailed', (r) =>
                result.console.push({ type: 'requestfailed', text: r.url() + ' ' + (r.failure() || {}).errorText }),
            );
            await page.setCookie({ name: 'PVEAuthCookie', value: tk, domain: host, path: '/', secure: true });
            await page.evaluateOnNewDocument((c) => {
                try {
                    sessionStorage.setItem('CSRFPreventionToken', c);
                } catch (_e) {}
            }, csrf);
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
                break;
            }
            await page.close();
            if (attempt === 5) throw new Error('SPA never rendered');
        }

        await openTab(page, 'lxc', vmid);
        result.checks.rowsBefore = await rows(page);

        // --- 1. Add, through the Add Key window -----------------------------
        // The window's Add is the write: `PUT ?view=added.by.ui&mode=replace`, and
        // `view::replace` creates `added` and `added.by` on the way. The panel
        // reloads afterwards, so the rows below are the server's answer.
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

        // --- 2. Edit that value, through the row editor ---------------------
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

        // --- 3. Remove: `DELETE ?view=added&digest=` ------------------------
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

        // --- 4. The Text card's Apply sends the buffer ----------------------
        // The one write that is text rather than a subtree: a `#` comment is not part
        // of the document model, so it survives only because this path sends what was
        // typed.
        await page.evaluate(() => {
            Ext.ComponentQuery.query('pveMetaTreePanel')[0].down('#modeBtn').setValue('text');
        });
        await sleep(9000);
        result.checks.textApply = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            if (!p.textEditor) return 'no editor';
            p.textEditor.setValue('# written from the text card\n' + p.textEditor.getValue() +
                'applied_from_text: yes\n');
            p.applyText();
            return 'applied';
        });
        await sleep(4000);
        await page.screenshot({ path: `${out}/extjs-text-apply.png` });
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
        result.checks.monacoDisposed = await page.evaluate(
            () => window.monaco.editor.getModels().length,
        );
        await api(
            `/api2/json/meta/guests/${vmid}?view=applied_from_text`,
            'DELETE',
            tk,
            csrf,
            '',
        );

        // --- 5. A concurrent write is a 409: one message, and a reload -------
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
        // The panel still holds the digest from before that write.
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
        await page.screenshot({ path: `${out}/extjs-conflict.png` });
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
        await page.screenshot({ path: `${out}/extjs-after-reload.png` });

        await api(
            `/api2/json/meta/guests/${vmid}?view=reload_probe`,
            'DELETE',
            tk,
            csrf,
            '',
        );

        // --- 5. Document keys colliding with Object.prototype members (S3) ---
        // `constructor`/`toString`/`hasOwnProperty` are ordinary, unreserved
        // document keys (DESIGN §7). A throwaway subtree, deleted again below.
        await api(
            `/api2/json/meta/guests/${vmid}`,
            'PUT',
            tk,
            csrf,
            'view=protokeys&mode=replace&data=' +
                encodeURIComponent(
                    JSON.stringify({
                        constructor: 'ctor-value',
                        toString: 'tostring-value',
                        hasOwnProperty: 'hop-value',
                    }),
                ),
        );
        await sleep(500);
        await page.evaluate(() => Ext.ComponentQuery.query('pveMetaTreePanel')[0].reload());
        await sleep(2500);
        result.checks.protoKeys = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            const r = {};
            p.getRootNode().cascadeBy((n) => {
                if (n.data.path && n.data.path.indexOf('protokeys.') === 0) {
                    r[n.data.path] = n.data.valueText;
                }
            });
            return {
                rows: r,
                // Nothing in the fix should ever reach the page's global Object.
                objectIntact:
                    typeof window.Object === 'function' && typeof window.Object.create === 'function',
            };
        });
        await page.screenshot({ path: `${out}/extjs-protokeys.png` });
        await api(`/api2/json/meta/guests/${vmid}?view=protokeys`, 'DELETE', tk, csrf, '');
        await sleep(500);

        // --- 6. The datacenter tab: the prefix registry list, and no document -----
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

        result.console = result.console.filter((m) => !/Mapping.Audit/.test(m.text));
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
