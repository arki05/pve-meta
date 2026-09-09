// headless-tab-check.js — headless verification of PVE.meta.TreePanel inside the real
// pve-manager SPA on node1, in both themes.
//
// Usage: node headless-tab-check.js <host> <vmid> <theme: light|dark>
//            [--stub-registry] [--readonly] [--scoped]
// --stub-registry stubs GET /meta/namespaces and GET /meta/grants, for a lab
//   whose real files do not exercise every schema shape.
// --readonly skips everything that writes.
// --scoped / --ro stub GET /meta/access with a restricted answer (an rw scope on
//   `traefik` only, or full read and no write) so the "Scoped write access" and
//   "Read-only" toolbar labels and the per-row editability can be seen without
//   depending on a second lab principal's credentials. Both imply --readonly.
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const theme = process.argv[4] || 'light';
const stubRegistry = process.argv.includes('--stub-registry');
const scoped = process.argv.includes('--scoped');
const roOnly = process.argv.includes('--ro');
const readOnly = scoped || roOnly || process.argv.includes('--readonly');
const ACCESS_STUB = scoped
    ? { read: 1, write: 0, scopes: [{ prefix: 'traefik', mode: 'rw' }] }
    : { read: 1, write: 0, scopes: [] };
const out = '/root/headless/shots';

const NAMESPACES = [
    {
        prefix: 'traefik',
        description: 'Traefik dynamic configuration',
        selector: { all: true },
        schema: {
            type: 'object',
            properties: {
                spec: {
                    type: 'object',
                    properties: {
                        host: { type: 'string', description: 'Public host name' },
                        port: { type: 'integer', default: 80, description: 'Backend port' },
                        scheme: { type: 'string', enum: ['http', 'https'], default: 'http' },
                        tls: { type: 'boolean', default: false, description: 'Terminate TLS' },
                    },
                },
            },
        },
    },
    { prefix: 'netbird', description: 'NetBird peer groups', selector: { tag: 'netbird' } },
];

const GRANTS = [
    {
        name: 'traefik',
        authid: 'svc@pve!traefik',
        description: 'Traefik dynamic-configuration provider',
        grants: [{ prefix: 'traefik', mode: 'rw', selector: { all: true } }],
    },
    {
        name: 'netbird',
        authid: 'svc@pve!netbird',
        description: 'NetBird peer group assignment',
        grants: [{ prefix: 'netbird', mode: 'ro', selector: { tag: 'netbird' } }],
    },
    {
        name: 'audit',
        authid: 'svc@pve!audit',
        description: 'Read-only observer of every guest',
        grants: [{ prefix: 'traefik', mode: 'ro', selector: { all: true } }],
    },
];

const ticket = () =>
    new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            {
                host,
                port: 8006,
                path: '/api2/json/access/ticket',
                method: 'POST',
                rejectUnauthorized: false,
                headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length },
            },
            (r) => {
                let d = '';
                r.on('data', (c) => (d += c));
                r.on('end', () => {
                    try {
                        resolve(JSON.parse(d).data);
                    } catch (_e) {
                        reject(new Error('ticket parse failed: ' + d));
                    }
                });
            },
        );
        req.on('error', reject);
        req.write(body);
        req.end();
    });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The DOM cell of one row, by column index (0 Key, 1 Value, 2 Description, 3 Access).
const cellHandle = (page, path, col) =>
    page.evaluateHandle(
        (pth, c) => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            let target = null;
            p.getRootNode().cascadeBy((n) => {
                if (n.data.path === pth) {
                    target = n;
                }
            });
            if (!target) {
                return null;
            }
            const row = p.tree.getView().getNode(target);
            return row ? row.querySelectorAll('.x-grid-cell')[c] : null;
        },
        path,
        col,
    );

async function main() {
    const t = await ticket();
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1500,950'],
        defaultViewport: { width: 1500, height: 950 },
    });
    const result = { theme, vmid, console: [], checks: {} };

    try {
        let page;
        for (let attempt = 1; attempt <= 5; attempt++) {
            page = await browser.newPage();
            page.on('console', (m) => result.console.push({ type: m.type(), text: m.text() }));
            page.on('pageerror', (e) => result.console.push({ type: 'pageerror', text: e.message }));
            page.on('requestfailed', (r) =>
                result.console.push({ type: 'requestfailed', text: r.url() + ' ' + (r.failure() || {}).errorText }),
            );

            await page.setCookie(
                { name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true },
                {
                    name: 'PVEThemeCookie',
                    value: theme === 'dark' ? 'proxmox-dark' : 'crisp',
                    domain: host,
                    path: '/',
                    secure: true,
                },
            );
            await page.evaluateOnNewDocument((csrf) => {
                try {
                    sessionStorage.setItem('CSRFPreventionToken', csrf);
                } catch (_e) {}
            }, t.CSRFPreventionToken);

            if (stubRegistry || scoped || roOnly) {
                await page.setRequestInterception(true);
                page.on('request', (req) => {
                    const reply = (data) =>
                        req.respond({
                            status: 200,
                            contentType: 'application/json',
                            body: JSON.stringify({ success: 1, data }),
                        });
                    if (stubRegistry && /\/api2\/(extjs|json)\/meta\/namespaces/.test(req.url())) {
                        reply(NAMESPACES);
                        return;
                    }
                    if (stubRegistry && /\/api2\/(extjs|json)\/meta\/grants/.test(req.url())) {
                        reply(GRANTS);
                        return;
                    }
                    if ((scoped || roOnly) && /\/api2\/(extjs|json)\/meta\/access/.test(req.url())) {
                        reply(ACCESS_STUB);
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
                break;
            }
            await page.close();
            if (attempt === 5) throw new Error('SPA never rendered');
        }

        // Select the guest in the resource tree.
        const handle = await page.evaluateHandle((id) => {
            return new Promise((resolve, reject) => {
                const tree = Ext.ComponentQuery.query('treepanel')[0];
                const rec =
                    tree.getStore().getNodeById('lxc/' + id) || tree.getStore().getNodeById('qemu/' + id);
                if (!rec) {
                    reject(new Error('no guest ' + id + ' in the resource tree'));
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
        }, vmid);
        await handle.asElement().click();
        await sleep(1500);

        // Click the "Metadata (ExtJS)" nav item the pve-ext loader should have added.
        const navHandle = await page.evaluateHandle(() =>
            Array.from(document.querySelectorAll('.x-treelist-item-text')).find(
                (e) => e.textContent.trim() === 'Metadata (ExtJS)',
            ),
        );
        result.checks.navItemPresent = !!navHandle.asElement();
        if (navHandle.asElement()) {
            await navHandle.asElement().click();
            await sleep(6000);
        }

        // How the loader resolves the manifest's xtype, and whether the class landed.
        result.checks.resolution = await page.evaluate(() => ({
            isCreatedByXtype: !!Ext.ClassManager.isCreated('pveMetaTreePanel'),
            isCreatedByClass: !!Ext.ClassManager.isCreated('PVE.meta.TreePanel'),
            nameByAlias: Ext.ClassManager.getNameByAlias('widget.pveMetaTreePanel') || null,
            instances: Ext.ComponentQuery.query('pveMetaTreePanel').length,
        }));

        // If the loader could not instantiate it, do by hand exactly what it would
        // have done: add the xtype into the wrapper panel it created for the tab.
        if (!result.checks.resolution.instances) {
            result.checks.manualInject = await page.evaluate((id) => {
                const wrap = Ext.ComponentQuery.query('#pve-ext-pve-meta-extjs')[0];
                if (!wrap) return 'no wrapper panel';
                wrap.removeAll(true);
                wrap.add({ xtype: 'pveMetaTreePanel', vmid: id, type: 'lxc' });
                return 'injected into wrapper';
            }, vmid);
            await sleep(6000);
        }

        result.checks.panel = await page.evaluate(() => {
            const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
            if (!p) return null;
            const rows = [];
            p.getRootNode().cascadeBy((n) => {
                if (n.data.path)
                    rows.push({
                        path: n.data.path,
                        value: n.data.valueText,
                        desc: n.data.description,
                        grammarDesc: n.data.grammarDescription,
                        access: n.data.accessText,
                        accessList: n.data.accessList,
                        icon: n.data.iconCls,
                        expandedCls: n.data.expandedCls,
                        present: n.data.present,
                        kind: n.data.kind,
                        editable: n.data.editable,
                        def: n.data.defaultValue,
                    });
            });
            const tb = p.getDockedItems('toolbar[dock=top]')[0];
            return {
                vmid: p.vmid,
                dc: p.dc,
                mode: p.mode,
                digest: p.digest,
                access: p.access,
                namespaces: (p.namespaces || []).length,
                grants: (p.grants || []).length,
                columns: p.tree.getColumns().map((c) => c.text),
                rows,
                toolbar: tb ? tb.items.items.map((i) => i.text || i.xtype) : [],
                // Ext.toolbar.TextItem has setText() but no getText(), and its `text`
                // property is not what ends up in the DOM - read the element.
                accessLabel: p.down('#accessText').isVisible()
                    ? p.down('#accessText').el.dom.textContent.trim()
                    : null,
                buttons: ['addBtn', 'editBtn', 'removeBtn', 'textSelBtn'].reduce((acc, id) => {
                    acc[id] = p.down('#' + id).isDisabled();
                    return acc;
                }, {}),
            };
        });

        const shotName = scoped ? 'scoped' : roOnly ? 'readonly' : 'tree';
        await page.screenshot({
            path: `${out}/extjs-${shotName}-${theme}${stubRegistry ? '-registry' : ''}.png`,
        });

        // The Access tooltip: hover the Access cell of a covered row.
        {
            // The row with the most entries, so the tooltip shows a real list.
            const target = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                let t = null;
                p.getRootNode().cascadeBy((n) => {
                    const len = (n.data.accessList || []).length;
                    if (len && (!t || len > t.data.accessList.length)) t = n;
                });
                return t ? t.data.path : null;
            });
            result.checks.accessTipRow = target;
            if (target) {
                const h = await cellHandle(page, target, 3);
                if (h.asElement()) {
                    const box = await h.asElement().boundingBox();
                    await page.mouse.move(box.x + box.width / 3, box.y + box.height / 2);
                    await sleep(1600);
                    result.checks.accessTip = await page.evaluate(() => {
                        const el = document.querySelector('.x-tip:not([style*="display: none"])');
                        return el ? el.innerText.replace(/\s+/g, ' ').trim() : null;
                    });
                    await page.screenshot({ path: `${out}/extjs-access-tip-${theme}.png` });
                    await page.mouse.move(5, 5);
                    await sleep(500);
                }
            }
        }

        // The row editor, opened the way a user opens it: a double-click on the row.
        {
            const target = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                let t = null;
                p.getRootNode().cascadeBy((n) => {
                    if (!t && n.data.kind !== 'map' && n.data.path && n.data.editable) t = n;
                });
                return t ? t.data.path : null;
            });
            const h = target && (await cellHandle(page, target, 0));
            if (h && h.asElement()) {
                await h.asElement().click({ clickCount: 2 });
                await sleep(900);
                result.checks.rowEditor = await page.evaluate(() => {
                    const w = Ext.ComponentQuery.query('pveMetaEditValueWindow')[0];
                    if (!w) return { open: false };
                    return {
                        open: true,
                        title: w.title,
                        field: w.down('#valueField').getXType(),
                        value: w.down('#valueField').getValue(),
                    };
                });
                await page.screenshot({ path: `${out}/extjs-rowedit-${theme}.png` });
                await page.evaluate(() =>
                    Ext.ComponentQuery.query('pveMetaEditValueWindow').forEach((w) => w.close()),
                );
                await sleep(400);
            }
        }

        // Row edits, the 409 path, and Monaco - all of which write.
        if (!stubRegistry && !readOnly) {
            // One write per value type, each through the row editor window.
            const editRow = async (path, value) => {
                const started = await page.evaluate(
                    (pth, v) => {
                        const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                        let target = null;
                        p.getRootNode().cascadeBy((n) => {
                            if (n.data.path === pth) target = n;
                        });
                        if (!target) return { result: 'row not found' };
                        p.setSelection(target);
                        p.editRow(target);
                        const w = Ext.ComponentQuery.query('pveMetaEditValueWindow')[0];
                        if (!w) return { result: 'no editor window' };
                        const xtype = w.down('#valueField').getXType();
                        w.down('#valueField').setValue(v);
                        w.submit();
                        return { result: 'edited', editor: xtype };
                    },
                    path,
                    value,
                );
                await sleep(2500);
                const after = await page.evaluate((pth) => {
                    const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                    let v = null;
                    p.getRootNode().cascadeBy((n) => {
                        if (n.data.path === pth) v = n.data.valueText;
                    });
                    return v;
                }, path);
                return { path, ...started, after };
            };

            result.checks.edits = [];
            result.checks.edits.push(await editRow('traefik.spec.host', 'ct200.edited.example'));
            result.checks.edits.push(await editRow('traefik.spec.port', 8081));
            result.checks.edits.push(await editRow('traefik.enabled', false));
            result.checks.edits.push(await editRow('netbird.groups', '["lan","dmz"]'));

            // The 409 path: send a write with a stale digest and read the dialog.
            result.checks.conflict = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                p.write({ view: 'traefik.spec.host', mode: 'replace', data: '"x"', digest: 'deadbeef' });
                return 'sent';
            });
            await sleep(2500);
            result.checks.conflictMsg = await page.evaluate(() => {
                const el = document.querySelector('.x-message-box');
                return el ? el.innerText.replace(/\s+/g, ' ').trim() : null;
            });
            await page.screenshot({ path: `${out}/extjs-conflict-${theme}.png` });
            await page.evaluate(() => {
                const b = Ext.ComponentQuery.query('messagebox')[0];
                if (b) b.hide();
            });
            await sleep(500);

            // "Edit selection as text": Monaco on the traefik subtree, in its window.
            result.checks.textSelDisabledWithoutSelection = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                p.tree.getSelectionModel().deselectAll();
                p.syncButtons();
                return p.down('#textSelBtn').isDisabled();
            });
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
            result.checks.selectionText = await page.evaluate(() => {
                const w = Ext.ComponentQuery.query('pveMetaTextWindow')[0];
                return {
                    open: !!w,
                    title: w ? w.title : null,
                    monacoLoaded: !!(window.monaco && window.monaco.editor),
                    jsyamlLoaded: !!(window.jsyaml && window.jsyaml.load),
                    text: w && w.editor ? w.editor.getValue() : null,
                };
            });
            await page.screenshot({ path: `${out}/extjs-selection-text-${theme}.png` });

            // Toggle to JSON, then show the diff on a changed buffer.
            result.checks.toggle = await page.evaluate(() => {
                const w = Ext.ComponentQuery.query('pveMetaTextWindow')[0];
                if (!w || !w.editor) return 'no editor';
                w.lookupReference('langbtn').setValue('json');
                return { lang: w.lang, text: w.editor.getValue() };
            });
            await sleep(1200);
            await page.screenshot({ path: `${out}/extjs-selection-text-json-${theme}.png` });

            await page.evaluate(() => {
                const w = Ext.ComponentQuery.query('pveMetaTextWindow')[0];
                const v = JSON.parse(w.editor.getValue());
                v.spec.port = 9999;
                v.newkey = 'added in the text editor';
                w.editor.setValue(JSON.stringify(v, null, 2));
                w.showDiff();
            });
            await sleep(4000);
            result.checks.diffOpen = await page.evaluate(
                () => !!document.querySelector('.monaco-diff-editor'),
            );
            await page.screenshot({ path: `${out}/extjs-diff-${theme}.png` });
            await page.evaluate(() => {
                const d = Ext.ComponentQuery.query('#pveMetaDiffWindow')[0];
                if (d) d.close();
                Ext.ComponentQuery.query('pveMetaTextWindow').forEach((w) => w.close());
            });
            await sleep(2500);
        }

        // The Tree | Text toggle: the whole document in Monaco, in the panel body.
        // With a stubbed access answer there is nothing to write, so only the state of
        // the toggle itself is checked.
        if (readOnly) {
            result.checks.textSegmentDisabled = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                return p.down('#modeBtn').items.getAt(1).isDisabled();
            });
        } else {
            await page.evaluate(() => {
                Ext.ComponentQuery.query('pveMetaTreePanel')[0].down('#modeBtn').setValue('text');
            });
            await sleep(9000);
            result.checks.textMode = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                return {
                    mode: p.mode,
                    hasEditor: !!p.textEditor,
                    text: p.textEditor ? p.textEditor.getValue() : null,
                    activeCard: p.getLayout().getActiveItem().itemId,
                    treeButtonsDisabled: ['addBtn', 'editBtn', 'removeBtn', 'textSelBtn', 'reloadBtn'].every(
                        (id) => p.down('#' + id).isDisabled(),
                    ),
                };
            });
            await page.screenshot({ path: `${out}/extjs-text-${theme}.png` });

            result.checks.textModeJson = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                p.down('#textLangBtn').setValue('json');
                return { lang: p.textLang, text: p.textEditor ? p.textEditor.getValue() : null };
            });
            await sleep(1200);
            await page.screenshot({ path: `${out}/extjs-text-json-${theme}.png` });

            // Leaving Text with an edited buffer must ask first.
            result.checks.dirtyGuard = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                p.down('#textLangBtn').setValue('yaml');
                p.textEditor.setValue(p.textEditor.getValue() + 'dirty_probe: 1\n');
                p.down('#modeBtn').setValue('tree');
                return { dirty: p.textIsDirty() };
            });
            await sleep(900);
            result.checks.dirtyGuardAsked = await page.evaluate(() => {
                const b = Ext.ComponentQuery.query('messagebox')[0];
                return b && b.isVisible() ? b.msgButtons.map((x) => x.text).join(',') : null;
            });
            await page.screenshot({ path: `${out}/extjs-text-dirty-${theme}.png` });
            await page.evaluate(() => {
                const b = Ext.ComponentQuery.query('messagebox')[0];
                if (b) {
                    const yes = b.query('button').find((x) => /yes/i.test(x.itemId || ''));
                    if (yes) yes.el.dom.click();
                }
            });
            await sleep(3000);
            result.checks.backToTree = await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                return {
                    mode: p.mode,
                    activeCard: p.getLayout().getActiveItem().itemId,
                    editorDisposed: !p.textEditor,
                    monacoModels: window.monaco ? window.monaco.editor.getModels().length : null,
                };
            });
        }

        result.console = result.console.filter(
            (m) =>
                (m.type === 'error' || m.type === 'pageerror' || m.type === 'requestfailed') &&
                // A pve-manager flake unrelated to this panel, filtered here as in
                // headless-flows-check.js.
                !/Mapping\.Audit/.test(m.text),
        );
    } catch (e) {
        result.error = e.message + '\n' + String(e.stack).split('\n').slice(0, 5).join('\n');
    } finally {
        await browser.close();
    }
    console.log(JSON.stringify(result, null, 2));
}

main().catch((e) => {
    console.error('FAILED: ' + e.stack);
    process.exit(1);
});
