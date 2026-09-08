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
  const t0 = Date.now();
  const logs = [];
  page.on('console', m => logs.push(`[+${Date.now()-t0}ms][${m.type()}] ${m.text()}`));
  page.on('pageerror', e => logs.push(`[+${Date.now()-t0}ms][pageerror] ${e.message}\n${e.stack}`));
  await page.setCookie({name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true});
  await page.evaluateOnNewDocument((csrf) => { sessionStorage.setItem('CSRFPreventionToken', csrf); }, t.CSRFPreventionToken);
  await page.goto(`https://${host}:8006/`, {waitUntil: 'networkidle2', timeout: 60000});
  await new Promise(r => setTimeout(r, 5000));
  console.log('AFTER INITIAL LOAD, before any click:');
  console.log('LOGS:', logs.join('\n---\n'));
  const navItemsBefore = await page.evaluate(() => Array.from(document.querySelectorAll('.x-treelist-item-text')).map(e => e.textContent.trim()));
  console.log('navItemsBefore click:', JSON.stringify(navItemsBefore));
  await browser.close();
})().catch(e => { console.error('FAIL', e); process.exit(1); });
