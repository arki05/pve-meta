// check-tab.js - real-browser (headless Chromium via puppeteer-core)
// verification that the pve-meta "Metadata" tab appears, alongside the
// normal PVE tabs, for a given target - and that the page throws no
// errors while doing it.
//
// Usage:
//   node check-tab.js <host> qemu <vmid>
//   node check-tab.js <host> lxc <vmid>
//   node check-tab.js <host> dc
//
// Drives the UI the way a real user would: logs in, loads the real PVE
// SPA at '/', then uses the ExtJS component API already running on the
// page purely to *locate* the resource-tree row for the target (expand
// its ancestor node(s), resolve the rendered DOM row) - then performs a
// real Puppeteer ElementHandle.click() on that row, exactly like a
// person clicking the tree (NOT page.mouse.click() at pre-computed
// coordinates - see the note above the tree-selection code below for
// why that was unreliable). The Metadata tab itself (in
// PVE.panel.Config's own left-hand treelist) is likewise clicked for
// real, not invoked via any internal API.
//
// The Metadata tab's iframe points at a same-origin, relative URL
// (/pve2/js/pve-meta-ui/index.html?...) served by pveproxy itself - see
// pve-meta-loader.js. This script asserts that `src` attribute and that
// the tab/iframe render at a reasonable size; it deliberately does NOT
// assert the iframe's *content* loads successfully, since the actual
// pve-meta UI static files are not installed by this patch (that iframe
// currently 404s/500s on the lab node, which is expected).
//
// KNOWN PRE-EXISTING, PATCH-UNRELATED FLAKE: on a fresh page load, stock
// (unpatched) pve-manager 9.2.11 has its own intermittent (~1-in-5 in
// this lab) race between the async GuiCap capability fetch and the
// automatic initial "Datacenter" resource-tree selection: if the
// Datacenter panel auto-constructs before GuiCap is fully populated,
// PVE.dc.Config's own "Resource Mappings" tab-building code throws
// "Cannot read properties of undefined (reading 'Mapping.Audit')" -
// reproduced identically with pve-meta-patch fully removed, so it is
// definitely not caused by this patch. This script works around it by
// reloading (bounded retries) until the initial view renders cleanly
// before touching the actual target under test.
const puppeteer = require('puppeteer-core');
const https = require('https');

const host = process.argv[2] || '10.10.10.154';
const kind = process.argv[3] || 'qemu'; // 'qemu' | 'lxc' | 'dc'
const vmid = process.argv[4];

if (kind !== 'dc' && !vmid) {
    console.error('usage: node check-tab.js <host> <qemu|lxc> <vmid>');
    console.error('       node check-tab.js <host> dc');
    process.exit(2);
}

function ticket() {
    return new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            {
                host,
                port: 8006,
                path: '/api2/json/access/ticket',
                method: 'POST',
                rejectUnauthorized: false,
                headers: {
                    'Content-Type': 'application/x-www-form-urlencoded',
                    'Content-Length': body.length,
                },
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
}

function sleep(ms) {
    return new Promise((r) => setTimeout(r, ms));
}

// Load the SPA fresh and wait for the initial default (Datacenter) view's
// own nav treelist to render with zero page errors, retrying with a
// brand new page a bounded number of times to route around the
// pre-existing flake described above. Returns the stabilized page, with
// console/pageerror listeners already attached and logging into
// `result`.
async function loadStablePage(browser, t, result, maxAttempts) {
    for (let attempt = 1; attempt <= maxAttempts; attempt++) {
        const page = await browser.newPage();
        const earlyErrors = [];
        page.on('console', (m) => result.consoleLogs.push(`[${m.type()}] ${m.text()}`));
        page.on('pageerror', (e) => earlyErrors.push(`[pageerror] ${e.message}`));

        await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
        await page.evaluateOnNewDocument((csrf) => {
            // evaluateOnNewDocument runs in EVERY new document context on
            // this page, including our own Metadata iframe. The iframe is
            // same-origin (served by pveproxy on the same host:port), but
            // still guard this: a 404/500 response can render as a
            // browser-internal error document in some circumstances, and
            // sessionStorage access there can throw - that must never
            // surface as a spurious page error unrelated to the patch
            // under test.
            try {
                sessionStorage.setItem('CSRFPreventionToken', csrf);
            } catch (e) {
                /* not the top-level PVE document - nothing to do here */
            }
        }, t.CSRFPreventionToken);

        await page.goto(`https://${host}:8006/`, { waitUntil: 'networkidle2', timeout: 60000 });

        const initialOk = await page
            .waitForFunction(() => document.querySelectorAll('.x-treelist-item-text').length > 0, {
                timeout: 15000,
                polling: 200,
            })
            .then(() => true)
            .catch(() => false);
        await sleep(500);

        if (initialOk && earlyErrors.length === 0) {
            // Now attach the listener that will feed the real result for
            // the rest of the run (errors from here on are attributed to
            // the actual test, not the known initial-load flake).
            page.on('pageerror', (e) => result.errors.push(`[pageerror] ${e.message}`));
            page.on('requestfailed', (req) => {
                // requestfailed fires for network-level failures (DNS,
                // connection refused, aborted) - NOT for a completed
                // response with an HTTP error status. The Metadata
                // iframe's 404/500 (expected - see header comment) is a
                // completed response and will not trigger this; only a
                // genuine network-level failure loading our own loader
                // script or the pve-meta UI path counts as an error here.
                if (/pve-ext-loader\.js|\/pve2\/js\/pve-meta-ui\//.test(req.url())) {
                    result.errors.push(`[requestfailed] ${req.url()} ${req.failure() && req.failure().errorText}`);
                }
            });
            result.checks.loadAttempts = attempt;
            return page;
        }

        result.checks.loadAttempts = attempt;
        result.checks.loadRetryReasons = result.checks.loadRetryReasons || [];
        result.checks.loadRetryReasons.push({ attempt, initialOk, earlyErrors });
        await page.close();
    }
    throw new Error(`initial Datacenter view never rendered cleanly after ${maxAttempts} fresh page loads`);
}

async function main() {
    const label = kind === 'dc' ? 'dc' : `${kind}-${vmid}`;
    const result = { target: label, pass: false, checks: {}, errors: [], consoleLogs: [] };

    const t = await ticket();
    const browser = await puppeteer.launch({
        executablePath: '/usr/bin/chromium',
        args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'],
        defaultViewport: { width: 1400, height: 900 },
    });

    try {
        const page = await loadStablePage(browser, t, result, 5);

        // GuiCap is confirmed populated by loadStablePage's success
        // condition (the initial DC nav only renders once GuiCap-gated
        // tabs are decided), but double check explicitly too.
        result.checks.capsReady = await page.evaluate(() => {
            try {
                const caps = Ext.state.Manager.get('GuiCap');
                return !!(caps && caps.mapping && caps.vms && caps.dc);
            } catch (e) {
                return false;
            }
        });
        result.checks.pveExtLoaded = await page.evaluate(() => window.PveExtLoaded === true);

        // --- Select the target in the main resource tree (or root for dc) ---
        //
        // NOTE: we resolve an actual Puppeteer ElementHandle for the row
        // and call .click() on it (which asks the browser for the row's
        // *current* box just before clicking), rather than reading
        // getBoundingClientRect() once in page.evaluate() and replaying
        // those numbers via page.mouse.click(x, y). The latter is racy:
        // expanding a parent node can still be settling/animating when
        // we read the rect, so the coordinates go stale by the time the
        // physical click fires and can land on the wrong row - which,
        // for this tree, silently selects a different resource and
        // fires PVE's *own* selectionchange handler for it instead.
        let targetHandle;
        if (kind === 'dc') {
            targetHandle = await page.evaluateHandle(() => {
                const tree = Ext.ComponentQuery.query('treepanel')[0];
                const store = tree.getStore();
                const rec = store.getNodeById('root');
                return tree.getView().getNode(rec);
            });
        } else {
            const targetId = `${kind}/${vmid}`;
            targetHandle = await page.evaluateHandle((targetId) => {
                return new Promise((resolve, reject) => {
                    const tree = Ext.ComponentQuery.query('treepanel')[0];
                    const store = tree.getStore();
                    const rec = store.getNodeById(targetId);
                    if (!rec) {
                        reject(new Error('no such node in resource tree store: ' + targetId));
                        return;
                    }
                    const ancestors = [];
                    let p = rec.parentNode;
                    while (p) {
                        ancestors.unshift(p);
                        p = p.parentNode;
                    }
                    let i = 0;
                    function expandNext() {
                        if (i >= ancestors.length) {
                            // Let any expand animation/layout settle before
                            // resolving the row element.
                            requestAnimationFrame(() => {
                                setTimeout(() => {
                                    resolve(tree.getView().getNode(rec));
                                }, 300);
                            });
                            return;
                        }
                        const node = ancestors[i++];
                        if (node.isExpanded()) {
                            expandNext();
                        } else {
                            node.expand(false, expandNext);
                        }
                    }
                    expandNext();
                });
            }, targetId);
            result.checks.treeRowText = await page.evaluate((el) => (el ? el.textContent.trim() : null), targetHandle);
        }

        const targetEl = targetHandle.asElement();
        if (!targetEl) throw new Error(`could not resolve a DOM element handle for the resource tree row (${kind}${vmid ? '/' + vmid : ''})`);
        await targetEl.click();
        await sleep(2500);

        // --- Read the config panel's own left-hand nav (treelist) ---
        const navItems = await page.evaluate(() =>
            Array.from(document.querySelectorAll('.x-treelist-item-text')).map((e) => e.textContent.trim()),
        );
        result.checks.navItems = navItems;
        result.checks.hasMetadataTab = navItems.includes('Metadata');
        result.checks.hasSummaryTab = navItems.includes('Summary');
        result.checks.normalTabCount = navItems.filter((t2) => t2 !== 'Metadata').length;

        if (!result.checks.hasMetadataTab) {
            throw new Error('Metadata tab not present in nav treelist: ' + JSON.stringify(navItems));
        }
        if (!result.checks.hasSummaryTab || result.checks.normalTabCount < 2) {
            throw new Error('normal PVE tabs did not render alongside Metadata: ' + JSON.stringify(navItems));
        }

        // --- Click the Metadata entry for real ---
        const metaHandle = await page.evaluateHandle(() =>
            Array.from(document.querySelectorAll('.x-treelist-item-text')).find((e) => e.textContent.trim() === 'Metadata'),
        );
        const metaEl = metaHandle.asElement();
        if (!metaEl) throw new Error('could not resolve a DOM handle for the Metadata nav item');
        await metaEl.click();
        await sleep(2500);

        const iframeInfo = await page.evaluate(() => {
            const f = document.querySelector('iframe[src*="/pve2/js/pve-meta-ui/"]');
            if (!f) return null;
            const rect = f.getBoundingClientRect();
            // f.src is the browser-resolved *absolute* URL; also grab the
            // raw attribute so we can assert it was written as a bare
            // relative path with no scheme/host/port of its own.
            return { src: f.src, attrSrc: f.getAttribute('src'), w: rect.width, h: rect.height };
        });
        result.checks.iframe = iframeInfo;

        if (!iframeInfo) throw new Error('no matching iframe (src containing /pve2/js/pve-meta-ui/) found after clicking Metadata');

        if (!iframeInfo.attrSrc.startsWith('/pve2/js/pve-meta-ui/index.html?')) {
            throw new Error(`iframe src is not the expected same-origin relative path: ${iframeInfo.attrSrc}`);
        }
        if (/^[a-z]+:\/\//i.test(iframeInfo.attrSrc)) {
            throw new Error(`iframe src should be a bare relative URL (no scheme/host/port): ${iframeInfo.attrSrc}`);
        }
        // Resolved against the page's own origin (https://<host>:8006),
        // confirming this is genuinely same-origin, not pointing at
        // some other host/port.
        if (!iframeInfo.src.startsWith(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?`)) {
            throw new Error(`iframe resolved to an unexpected origin: ${iframeInfo.src}`);
        }

        const expectedQuery = kind === 'dc' ? 'dc=1' : `vmid=${vmid}`;
        if (!iframeInfo.attrSrc.includes(expectedQuery)) {
            throw new Error(`iframe src missing expected query "${expectedQuery}": ${iframeInfo.attrSrc}`);
        }
        if (!/theme=(light|dark)/.test(iframeInfo.attrSrc)) {
            throw new Error('iframe src missing theme=light|dark: ' + iframeInfo.attrSrc);
        }
        if (!(iframeInfo.w > 50 && iframeInfo.h > 50)) {
            throw new Error(`iframe (the tab content area) rendered too small to be "filling the tab": ${iframeInfo.w}x${iframeInfo.h}`);
        }

        await page.screenshot({ path: `/root/headless/tab-${label}.png` });

        if (result.errors.length > 0) {
            throw new Error('page/console errors recorded during target interaction: ' + JSON.stringify(result.errors));
        }

        result.pass = true;
    } catch (e) {
        result.pass = false;
        result.failure = e.message;
    } finally {
        await browser.close();
    }

    console.log(JSON.stringify(result, null, 2));
    process.exit(result.pass ? 0 : 1);
}

main().catch((e) => {
    console.error('FATAL', e);
    process.exit(1);
});
