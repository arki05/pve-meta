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
  page.on('pageerror', e => logs.push(`[+${Date.now()-t0}ms][pageerror] ${e.message}\n${e.stack}`));
  await page.setCookie({name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true});
  await page.evaluateOnNewDocument((csrf) => { sessionStorage.setItem('CSRFPreventionToken', csrf); }, t.CSRFPreventionToken);
  await page.goto(`https://${host}:8006/`, {waitUntil: 'networkidle2', timeout: 60000});
  await sleep(4000);

  const targetId = 'qemu/300';
  const rowInfo = await page.evaluate((targetId) => {
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
          const row = view.getNode(rec);
          const rect = row.getBoundingClientRect();
          resolve({found: true, x: rect.x + rect.width/2, y: rect.y + rect.height/2, text: rec.get('text')});
          return;
        }
        const node = ancestors[i++];
        if (node.isExpanded()) expandNext(); else node.expand(false, expandNext);
      }
      expandNext();
    });
  }, targetId);
  console.log('rowInfo:', JSON.stringify(rowInfo));

  await page.mouse.click(rowInfo.x, rowInfo.y);
  await sleep(500);
  console.log('LOGS right after click (+500ms):', logs.join('\n---\n'));
  await page.screenshot({path: '/root/headless/diag5-immediate.png'});

  await sleep(3000);
  console.log('LOGS after 3.5s total post-click:', logs.join('\n---\n'));
  await page.screenshot({path: '/root/headless/diag5-after.png'});

  const activeInfo = await page.evaluate(() => {
    try {
      const panels = Ext.ComponentQuery.query('pvePanelConfig');
      return {
        count: panels.length,
        classNames: panels.map(p => p.self ? p.self.getName() : p.$className),
        firstItemsCount: panels[0] ? Object.keys(panels[0].savedItems||{}).length : null,
      };
    } catch(e) { return {error: e.message}; }
  });
  console.log('activeInfo:', JSON.stringify(activeInfo));
  console.log('page url:', page.url());
  await browser.close();
})().catch(e => { console.error('FAIL', e); process.exit(1); });
