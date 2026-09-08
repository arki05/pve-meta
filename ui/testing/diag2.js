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
  await page.setCookie({name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true});
  await page.evaluateOnNewDocument((csrf) => { sessionStorage.setItem('CSRFPreventionToken', csrf); }, t.CSRFPreventionToken);
  await page.goto(`https://${host}:8006/`, {waitUntil: 'networkidle2', timeout: 60000});
  await new Promise(r => setTimeout(r, 4000));

  // Expand node/pvemeta-node1 via Ext API, then dump the rendered DOM
  // for the row corresponding to qemu/300, so we know what to click.
  const expandInfo = await page.evaluate(() => {
    return new Promise((resolve) => {
      const tree = Ext.ComponentQuery.query('treepanel')[0];
      const store = tree.getStore();
      const nodeRec = store.getNodeById('node/pvemeta-node1');
      nodeRec.expand(false, () => {
        const vmRec = store.getNodeById('qemu/300');
        const view = tree.getView();
        const row = view.getNode(vmRec);
        const rect = row ? row.getBoundingClientRect() : null;
        resolve({
          found: !!row,
          html: row ? row.outerHTML.slice(0, 500) : null,
          rect: rect ? {x: rect.x, y: rect.y, w: rect.width, h: rect.height} : null,
          classNames: row ? row.className : null,
        });
      });
    });
  });
  console.log('EXPAND_INFO:', JSON.stringify(expandInfo, null, 2));
  await page.screenshot({path: '/root/headless/diag2.png'});
  await browser.close();
})().catch(e => { console.error('FAIL', e); process.exit(1); });
