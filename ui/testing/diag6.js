const puppeteer = require('puppeteer-core');
const https = require('https');
const host = process.argv[2] || '10.10.10.154';
function ticket() {
  return new Promise((res, rej) => {
    const body = 'username=root%40pam&password=pvelab';
    const req = https.request({host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
      headers: {'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length}}, r => {
      let d=''; r.on('data', c => d+=c); r.on('end', () => res(JSON.parse(d).data));
    });
    req.on('error', rej); req.write(body); req.end();
  });
}
function sleep(ms){return new Promise(r=>setTimeout(r,ms));}
(async () => {
  const t = await ticket();
  const browser = await puppeteer.launch({executablePath: '/usr/bin/chromium', args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'], defaultViewport: {width: 1400, height: 900}});
  const page = await browser.newPage();
  const t0 = Date.now();
  const logs = [];
  page.on('console', m => logs.push(`[+${Date.now()-t0}ms][${m.type()}] ${m.text()}`));
  page.on('pageerror', e => logs.push(`[+${Date.now()-t0}ms][pageerror] ${e.message}`));
  await page.setCookie({name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true});
  await page.evaluateOnNewDocument((csrf) => { sessionStorage.setItem('CSRFPreventionToken', csrf); }, t.CSRFPreventionToken);
  await page.goto(`https://${host}:8006/`, {waitUntil: 'networkidle2', timeout: 60000});
  await sleep(4000);

  for (let attempt = 1; attempt <= 5; attempt++) {
    const targetId = 'qemu/300';
    const handle = await page.evaluateHandle((targetId) => {
      return new Promise((resolve) => {
        const tree = Ext.ComponentQuery.query('treepanel')[0];
        const store = tree.getStore();
        const rec = store.getNodeById(targetId);
        const ancestors = [];
        let p = rec.parentNode;
        while (p) { ancestors.unshift(p); p = p.parentNode; }
        let i = 0;
        function expandNext() {
          if (i >= ancestors.length) {
            const view = tree.getView();
            // settle: wait one animation frame + a tick for layout to
            // stabilize after any expand animation before resolving.
            requestAnimationFrame(() => {
              setTimeout(() => {
                const row = view.getNode(rec);
                resolve(row);
              }, 300);
            });
            return;
          }
          const node = ancestors[i++];
          if (node.isExpanded()) expandNext(); else node.expand(false, expandNext);
        }
        expandNext();
      });
    }, targetId);

    const el = handle.asElement();
    console.log(`attempt ${attempt}: got element handle:`, !!el);
    if (el) {
      await el.click();
    }
    await sleep(1500);

    const navItems = await page.evaluate(() => Array.from(document.querySelectorAll('.x-treelist-item-text')).map(e => e.textContent.trim()));
    const activeClass = await page.evaluate(() => {
      const p = Ext.ComponentQuery.query('pvePanelConfig')[0];
      return p ? (p.self ? p.self.getName() : p.$className) : null;
    });
    console.log(`attempt ${attempt}: activeClass=${activeClass} navItems.length=${navItems.length} hasMetadata=${navItems.includes('Metadata')}`);
  }

  console.log('LOGS:', logs.join('\n---\n'));
  await browser.close();
})().catch(e => { console.error('FAIL', e); process.exit(1); });
