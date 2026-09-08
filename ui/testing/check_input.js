const puppeteer = require('puppeteer-core');
const https = require('https');

const host = '10.10.10.154';
const vmid = '200';

function ticket() {
    return new Promise((resolve, reject) => {
        const body = 'username=root%40pam&password=pvelab';
        const req = https.request(
            { host, port: 8006, path: '/api2/json/access/ticket', method: 'POST', rejectUnauthorized: false,
              headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'Content-Length': body.length } },
            (r) => { let d = ''; r.on('data', (c) => (d += c)); r.on('end', () => resolve(JSON.parse(d).data)); }
        );
        req.on('error', reject); req.write(body); req.end();
    });
}
function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

async function main() {
    const t = await ticket();
    const browser = await puppeteer.launch({ executablePath: '/usr/bin/chromium', args: ['--no-sandbox','--ignore-certificate-errors'] });
    const page = await browser.newPage();
    await page.setCookie({ name: 'PVEAuthCookie', value: t.ticket, domain: host, path: '/', secure: true });
    await page.evaluateOnNewDocument((csrf) => { try { sessionStorage.setItem('CSRFPreventionToken', csrf); } catch (e) {} }, t.CSRFPreventionToken);
    await page.goto(`https://${host}:8006/pve2/js/pve-meta-ui/index.html?vmid=${vmid}`, { waitUntil: 'networkidle2', timeout: 60000 });
    await sleep(6000);
    const inputs = await page.evaluate(() => Array.from(document.querySelectorAll('input')).map(i => ({name: i.name, value: i.value, placeholder: i.placeholder})));
    console.log(JSON.stringify(inputs, null, 2));
    await browser.close();
}
main().catch(e => { console.error(e); process.exit(1); });
