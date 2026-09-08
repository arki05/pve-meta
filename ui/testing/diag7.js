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
  for (let run = 1; run <= 5; run++) {
    const page = await browser.newPage();
    const logs = [];
    page.on('pageerror', e => logs.push(`[pageerror] ${e.message}`));
    await page.setCookie({name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true});
    await page.evaluateOnNewDocument((csrf) => { sessionStorage.setItem('CSRFPreventionToken', csrf); }, t.CSRFPreventionToken);
    await page.goto(`https://${host}:8006/`, {waitUntil: 'networkidle2', timeout: 60000});
    // Wait for the INITIAL default (Datacenter) panel's own nav treelist
    // to actually finish rendering before doing anything else.
    const initialOk = await page.waitForFunction(
      () => document.querySelectorAll('.x-treelist-item-text').length > 0,
      { timeout: 15000, polling: 100 },
    ).then(() => true).catch(() => false);
    await sleep(300);
    const navCount = await page.evaluate(() => document.querySelectorAll('.x-treelist-item-text').length);
    console.log(`run ${run}: initialOk=${initialOk} navCount=${navCount} errors=${JSON.stringify(logs)}`);
    await page.close();
  }
  await browser.close();
})().catch(e => { console.error('FAIL', e); process.exit(1); });
