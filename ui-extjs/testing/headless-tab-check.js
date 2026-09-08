// extjs-tab-check.js — headless verification of PVE.meta.TreePanel inside the real
// pve-manager SPA on node1, in both themes.
//
// Usage: node extjs-tab-check.js <host> <vmid> <theme: light|dark> [--operators]
// --operators stubs a revision-5 GET /meta/operators payload (the endpoint is not
// deployed yet) so the Owner column and the declared-but-unset rows can be seen.
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const vmid = process.argv[3] || '200';
const theme = process.argv[4] || 'light';
const stubOperators = process.argv.includes('--operators');
const readOnly = process.argv.includes('--readonly');
const out = '/root/headless/shots';

const OPERATORS = [
    {
        name: 'traefik',
        authid: 'svc@pve!traefik',
        description: 'Traefik dynamic-configuration provider',
        scopes: [
            {
                prefix: 'traefik',
                mode: 'rw',
                selector: { all: true },
                grammar: {
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
        ],
    },
    {
        name: 'netbird',
        authid: 'svc@pve!netbird',
        description: 'NetBird peer group assignment',
        scopes: [{ prefix: 'netbird', mode: 'ro', selector: { tag: 'netbird' } }],
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
                    } catch (e) {
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
                } catch (e) {}
            }, t.CSRFPreventionToken);

            if (stubOperators) {
                await page.setRequestInterception(true);
                page.on('request', (req) => {
                    if (/\/api2\/(extjs|json)\/meta\/operators/.test(req.url())) {
                        req.respond({
                            status: 200,
                            contentType: 'application/json',
                            body: JSON.stringify({ success: 1, data: OPERATORS }),
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
                        owner: n.data.ownerText,
                        present: n.data.present,
                        kind: n.data.kind,
                        editable: n.data.editable,
                        desc: n.data.description,
                        def: n.data.defaultValue,
                    });
            });
            return {
                vmid: p.vmid,
                dc: p.dc,
                digest: p.digest,
                access: p.access,
                registrations: (p.registrations || []).length,
                rows,
                toolbar: p.getDockedItems('toolbar[dock=top]')[0]
                    ? p.getDockedItems('toolbar[dock=top]')[0].items.items.map((i) => i.text || i.xtype)
                    : [],
            };
        });

        await page.screenshot({ path: `${out}/extjs-tree-${theme}${stubOperators ? '-operators' : ''}.png` });

        // A cell editor opened but not committed - what inline editing looks like.
        {
            const h = await page.evaluateHandle(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                let t = null;
                p.getRootNode().cascadeBy((n) => {
                    if (!t && n.data.kind !== 'map' && n.data.path) t = n;
                });
                const row = t && p.getView().getNode(t);
                return row ? row.querySelectorAll('.x-grid-cell')[1] : null;
            });
            if (h.asElement()) {
                await h.asElement().click();
                await sleep(900);
                result.checks.openEditor = await page.evaluate(() => {
                    const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                    const ed = p.cellEditing.getActiveEditor();
                    return ed ? ed.field.getXType() : null;
                });
                await page.screenshot({ path: `${out}/extjs-celledit-${theme}.png` });
                await page.evaluate(() =>
                    Ext.ComponentQuery.query('pveMetaTreePanel')[0].cellEditing.cancelEdit(),
                );
                await sleep(400);
            }
        }

        // Inline edits, the 409 path, and Monaco - all of which write.
        if (!stubOperators && !readOnly) {
            // Inline edits, one per value type, each clicked like a user would.
            const editRow = async (path, value) => {
                const h = await page.evaluateHandle((pth) => {
                    const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                    let target = null;
                    p.getRootNode().cascadeBy((n) => {
                        if (n.data.path === pth) target = n;
                    });
                    if (!target) return null;
                    const row = p.getView().getNode(target);
                    return row ? row.querySelectorAll('.x-grid-cell')[1] : null;
                }, path);
                if (!h.asElement()) return { path, result: 'row not found' };
                await h.asElement().click();
                await sleep(900);
                const started = await page.evaluate((v) => {
                    const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                    const ed = p.cellEditing.getActiveEditor();
                    if (!ed) return { result: 'no active editor' };
                    const xtype = ed.field.getXType();
                    ed.field.setValue(v);
                    p.cellEditing.completeEdit();
                    return { result: 'edited', editor: xtype };
                }, value);
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

            // Monaco: the "Edit as Text" window on the traefik subtree.
            await page.evaluate(() => {
                const p = Ext.ComponentQuery.query('pveMetaTreePanel')[0];
                let n = null;
                p.getRootNode().cascadeBy((x) => {
                    if (x.data.path === 'traefik') n = x;
                });
                p.setSelection(n);
                p.editAsText();
            });
            await sleep(9000);
            result.checks.monaco = await page.evaluate(() => {
                const w = Ext.ComponentQuery.query('pveMetaTextWindow')[0];
                return {
                    open: !!w,
                    monacoLoaded: !!(window.monaco && window.monaco.editor),
                    text: w && w.editor ? w.editor.getValue() : null,
                    theme: w && w.editor ? null : null,
                };
            });
            await page.screenshot({ path: `${out}/extjs-monaco-${theme}.png` });

            // Toggle to JSON, then show the diff on a changed buffer.
            result.checks.toggle = await page.evaluate(() => {
                const w = Ext.ComponentQuery.query('pveMetaTextWindow')[0];
                if (!w || !w.editor) return 'no editor';
                w.lookupReference('langbtn').setValue('json');
                return { lang: w.lang, text: w.editor.getValue() };
            });
            await sleep(1200);
            await page.screenshot({ path: `${out}/extjs-monaco-json-${theme}.png` });

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
        }

        result.console = result.console.filter(
            (m) => m.type === 'error' || m.type === 'pageerror' || m.type === 'requestfailed',
        );
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
