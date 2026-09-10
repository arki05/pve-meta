// headless-flows-check.js — the flows headless-tab-check.js does not cover: applying
// the Monaco diff, Add, Remove, the 5 s version poll, and the datacenter document.
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

        // --- 2. Remove, through the row's trash action -----------------------
        result.checks.remove = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            let n = null;
            p.getRootNode().cascadeBy((x) => {
                if (x.data.path === 'added') n = x;
            });
            if (!n) return 'row not found';
            p.removeKey(n);
            return 'confirm shown';
        });
        await sleep(700);
        result.checks.confirmClicked = await page.evaluate(() => {
            const b = Ext.ComponentQuery.query('messagebox')[0];
            if (!b) return 'no messagebox';
            const yes = b.query('button').find((x) => /yes/i.test(x.itemId || '') || /^Yes$/i.test(x.text));
            if (!yes) return 'no yes button: ' + b.query('button').map((x) => x.itemId + '/' + x.text).join(',');
            yes.el.dom.click();
            return 'clicked';
        });
        await sleep(3000);
        result.checks.afterRemove = (await rows(page)).filter((r) => r.startsWith('added'));

        // --- 3. Apply from the selection text window, through the diff -------
        await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            let n = null;
            p.getRootNode().cascadeBy((x) => {
                if (x.data.path === 'traefik') n = x;
            });
            p.setSelection(n);
            p.editSelectionAsText();
        });
        await sleep(9000);
        result.checks.textWindow = await page.evaluate(() => {
            const w = Ext.ComponentQuery.query('pveMetaTextWindow')[0];
            if (!w || !w.editor) return 'no editor';
            w.editor.setValue(w.editor.getValue() + 'applied_from_text: yes\n');
            w.showDiff();
            return 'diff requested';
        });
        await sleep(4000);
        await page.screenshot({ path: `${out}/extjs-diff-apply.png` });
        result.checks.applyClick = await page.evaluate(() => {
            const confirm = Ext.ComponentQuery.query('#pveMetaDiffWindow')[0];
            if (!confirm) return 'no diff window';
            const btn = confirm.query('button').find((b) => b.text === 'Apply');
            if (!btn) return 'buttons seen: ' + confirm.query('button').map((b) => b.text).join(',');
            btn.el.dom.click();
            return 'clicked';
        });
        await sleep(4000);
        result.checks.afterApply = (await rows(page)).filter((r) => r.startsWith('traefik'));
        result.checks.textWindowClosed = await page.evaluate(
            () => Ext.ComponentQuery.query('pveMetaTextWindow').length === 0,
        );
        result.checks.monacoDisposed = await page.evaluate(
            () => window.monaco.editor.getModels().length,
        );

        // --- 4. The version poll: change the store from outside --------------
        await page.evaluate(() => Ext.ComponentQuery.query('pveMetaTextWindow').forEach((w) => w.close()));
        await sleep(1500);
        result.checks.windowsClosed = await page.evaluate(() => ({
            text: Ext.ComponentQuery.query('pveMetaTextWindow').length,
            diff: Ext.ComponentQuery.query('#pveMetaDiffWindow').length,
            models: window.monaco.editor.getModels().length,
        }));
        const before = await rows(page);
        await api(
            `/api2/json/meta/guests/${vmid}`,
            'PUT',
            tk,
            csrf,
            'view=poll_probe&mode=replace&data=' + encodeURIComponent('"set from outside"'),
        );
        await sleep(9000);
        const after = await rows(page);
        result.checks.poll = {
            appeared: after.some((r) => r.startsWith('poll_probe')),
            beforeCount: before.length,
            afterCount: after.length,
        };
        await page.screenshot({ path: `${out}/extjs-after-poll.png` });

        // The poll must never fire while the row editor is open.
        const rowCell = await page.evaluateHandle(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            let n = null;
            p.getRootNode().cascadeBy((x) => {
                if (!n && x.data.kind !== 'map' && x.data.path && x.data.editable) n = x;
            });
            const row = n && p.tree.getView().getNode(n);
            return row ? row.querySelectorAll('.x-grid-cell')[0] : null;
        });
        if (rowCell.asElement()) {
            await rowCell.asElement().click({ clickCount: 2 });
            await sleep(900);
        }
        result.checks.pollSuppressed = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            const editing = p.editing;
            const reloaded = [];
            const orig = p.reload;
            p.reload = function () {
                reloaded.push(1);
                return orig.apply(this, arguments);
            };
            p.poll();
            Ext.ComponentQuery.query('pveMetaEditValueWindow').forEach((w) => w.close());
            p.reload = orig;
            return { editingFlagSet: editing, reloadCalledWhileEditing: reloaded.length };
        });

        await api(
            `/api2/json/meta/guests/${vmid}?view=poll_probe`,
            'DELETE',
            tk,
            csrf,
            '',
        );

        // --- 5. Document keys colliding with Object.prototype members (S3) ---
        // `constructor`/`toString`/`hasOwnProperty` are ordinary, unreserved
        // document keys (DESIGN §4). A throwaway subtree, deleted again below.
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

        // --- 6. The datacenter document --------------------------------------
        await openTab(page, 'dc');
        result.checks.datacenter = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            if (!p) return 'no panel';
            const r = [];
            p.getRootNode().cascadeBy((n) => n.data.path && r.push(n.data.path));
            return { dc: p.dc, baseUrl: p.baseUrl, access: p.access, rows: r };
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
