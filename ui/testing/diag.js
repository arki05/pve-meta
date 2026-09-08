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
(async () => {
  const t = await ticket();
  const browser = await puppeteer.launch({executablePath: '/usr/bin/chromium', args: ['--no-sandbox', '--ignore-certificate-errors', '--window-size=1400,900'], defaultViewport: {width: 1400, height: 900}});
  const page = await browser.newPage();
  const logs = [];
  page.on('console', m => logs.push(`[${m.type()}] ${m.text()}`));
  page.on('pageerror', e => logs.push(`[pageerror] ${e.message}`));
  await page.setCookie({name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true});
  await page.evaluateOnNewDocument((csrf) => { sessionStorage.setItem('CSRFPreventionToken', csrf); }, t.CSRFPreventionToken);
  await page.goto(`https://${host}:8006/`, {waitUntil: 'networkidle2', timeout: 60000});
  await new Promise(r => setTimeout(r, 4000));

  // Use ExtJS's own API inside the page to find the resource tree.
  const info = await page.evaluate(() => {
    const out = {};
    try {
      out.extExists = typeof Ext !== 'undefined';
      const trees = Ext.ComponentQuery.query('treepanel');
      out.treeCount = trees.length;
      out.treeItemIds = trees.map(t => t.itemId || t.id);
      // try to find the main resource tree specifically
      const rtree = Ext.ComponentQuery.query('#tree')[0] || trees[0];
      if (rtree && rtree.getStore) {
        const store = rtree.getStore();
        const root = store.getRoot();
        function dump(node, depth) {
          if (depth > 3) return [];
          let arr = [{text: node.get ? node.get('text') : node.text, id: node.get ? node.get('id') : node.id, depth}];
          (node.childNodes || []).forEach(c => { arr = arr.concat(dump(c, depth+1)); });
          return arr;
        }
        out.nodes = dump(root, 0);
      }
    } catch (e) {
      out.error = e.message;
    }
    return out;
  });
  console.log('INFO:', JSON.stringify(info, null, 2));
  console.log('LOGS:', logs.slice(0,30).join('\n'));
  await page.screenshot({path: '/root/headless/diag.png'});
  await browser.close();
})().catch(e => { console.error('FAIL', e); process.exit(1); });
