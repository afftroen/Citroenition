// node page_report.mjs <browser-ws-url> <out-dir> <name> <url> [ready-regex] [max-wait-s] [settle-s]
// Opens <url> in a fresh tab of the browser behind <browser-ws-url>, waits until innerText matches
// [ready-regex] (and then stays stable for [settle-s]), writes <out-dir>/<name>.png (full page, capped) and
// <out-dir>/<name>.txt and prints one JSON line. Needs node >= 22 (global WebSocket). No secrets are logged.
import { writeFileSync, mkdirSync } from 'node:fs';
const [, , WS, OUT, NAME, URL_, READY = '.', MAXW = '90', SETTLE = '6'] = process.argv;
mkdirSync(OUT, { recursive: true });
const ws = new WebSocket(WS);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('cannot open ' + WS)); });
let id = 0; const pend = new Map(); const cons = [], fails = [], reqs = new Map();
const short = u => { try { const x = new globalThis.URL(u); return x.host + x.pathname.slice(0, 50); } catch { return String(u).slice(0, 80); } };
ws.onmessage = e => {
  const d = JSON.parse(e.data);
  if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); return; }
  const p = d.params || {};
  if (d.method === 'Network.requestWillBeSent') reqs.set(p.requestId, short(p.request.url));
  else if (d.method === 'Network.loadingFailed') fails.push(short(reqs.get(p.requestId)) + ' :: ' + p.errorText);
  else if (d.method === 'Runtime.exceptionThrown') cons.push('exception: ' + (p.exceptionDetails?.exception?.description ?? p.exceptionDetails?.text ?? '').slice(0, 160));
};
const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
const sleep = ms => new Promise(r => setTimeout(r, ms));
setTimeout(() => { console.error('page_report timeout'); process.exit(3); }, (Number(MAXW) + 60) * 1000);

const { targetId } = (await send('Target.createTarget', { url: 'about:blank' })).result;
const sid = (await send('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
await send('Page.enable', {}, sid); await send('Network.enable', {}, sid); await send('Runtime.enable', {}, sid);
await send('Emulation.setDeviceMetricsOverride', { width: 1366, height: 900, deviceScaleFactor: 1, mobile: false }, sid);
await send('Page.navigate', { url: URL_ }, sid);
const ev = async expr => {
  const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true }, sid);
  return r.result?.result?.value ?? ('ERR ' + (r.result?.exceptionDetails?.exception?.description ?? r.error?.message ?? 'unknown'));
};
const re = new RegExp(READY, 'i');
let text = '', ready = false, stable = 0, last = '';
const t0 = Date.now();
while (Date.now() - t0 < Number(MAXW) * 1000) {
  await sleep(2500);
  text = String(await ev('document.body ? document.body.innerText : ""'));
  if (re.test(text) && text.length > 100) {
    ready = true;
    if (text === last) { stable += 2.5; if (stable >= Number(SETTLE)) break; } else stable = 0;
    last = text;
  }
}
text = String(await ev('document.body ? document.body.innerText : ""'));
const m = await send('Page.getLayoutMetrics', {}, sid);
const cs = m.result?.cssContentSize || m.result?.contentSize || { width: 1366, height: 900 };
const h = Math.min(Math.ceil(cs.height), 7000);
const shot = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true, clip: { x: 0, y: 0, width: 1366, height: h, scale: 1 } }, sid);
if (shot.result?.data) writeFileSync(`${OUT}/${NAME}.png`, Buffer.from(shot.result.data, 'base64'));
writeFileSync(`${OUT}/${NAME}.txt`, text);
console.log(JSON.stringify({ name: NAME, ready, waited_s: Math.round((Date.now() - t0) / 1000), chars: text.length, page_h: h, failed: fails.slice(0, 10), exceptions: cons.slice(0, 8), head: text.slice(0, 700) }));
process.exit(0);
