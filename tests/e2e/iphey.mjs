// node iphey.mjs <browser-ws-url> <out-dir> [url]
// Opens https://iphey.com/ (or [url]) in a fresh tab of the browser behind <browser-ws-url>, waits until the
// test finished (the "Temporary value" placeholders are gone), then writes <out-dir>/iphey.png and iphey.txt and
// prints one JSON line with what the page says. Needs node >= 22 (global WebSocket).
import { writeFileSync, mkdirSync } from 'node:fs';
const [, , WS, OUT, URL_ = 'https://iphey.com/'] = process.argv;
mkdirSync(OUT, { recursive: true });
const ws = new WebSocket(WS);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('cannot open ' + WS)); });
let id = 0; const pend = new Map();
const reqs = new Map(), fails = [], cons = [];
const short = u => { try { const x = new globalThis.URL(u); return x.host + x.pathname.slice(0, 60); } catch { return String(u).slice(0, 80); } };
ws.onmessage = e => {
  const d = JSON.parse(e.data);
  if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); return; }
  const p = d.params || {};
  if (d.method === 'Network.requestWillBeSent') reqs.set(p.requestId, { url: short(p.request.url), t: Date.now() });
  else if (d.method === 'Network.responseReceived') { const r = reqs.get(p.requestId); if (r) r.status = p.response.status; }
  else if (d.method === 'Network.loadingFinished') { const r = reqs.get(p.requestId); if (r) r.done = true; }
  else if (d.method === 'Network.loadingFailed') fails.push(short(reqs.get(p.requestId)?.url) + ' :: ' + p.errorText);
  else if (d.method === 'Runtime.consoleAPICalled' && ['error', 'warning', 'assert'].includes(p.type)) cons.push(p.type + ': ' + (p.args || []).map(a => a.value ?? a.description ?? '').join(' ').slice(0, 200));
  else if (d.method === 'Runtime.exceptionThrown') cons.push('exception: ' + (p.exceptionDetails?.exception?.description ?? p.exceptionDetails?.text ?? '').slice(0, 200));
};
const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
const sleep = ms => new Promise(r => setTimeout(r, ms));
setTimeout(() => { console.error('iphey timeout'); process.exit(3); }, 150000);

const { targetId } = (await send('Target.createTarget', { url: 'about:blank' })).result;
const sid = (await send('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
await send('Page.enable', {}, sid);
await send('Network.enable', {}, sid);
await send('Runtime.enable', {}, sid);
await send('Emulation.setDeviceMetricsOverride', { width: 1366, height: 900, deviceScaleFactor: 1, mobile: false }, sid);
await send('Page.navigate', { url: URL_ }, sid);
const ev = async expr => {
  const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true }, sid);
  return r.result?.result?.value ?? ('ERR ' + (r.result?.exceptionDetails?.exception?.description ?? r.error?.message ?? 'unknown'));
};
let text = '', ready = false;
const t0 = Date.now();
while (Date.now() - t0 < 110000) {
  await sleep(2500);
  text = String(await ev('document.body ? document.body.innerText : ""'));
  // the five tiles show "Temporary value" until the fingerprint run finishes
  if (text.length > 200 && !/Temporary value/.test(text.split('Any Check You Need')[0])) { ready = true; break; }
  if (cons.some(c => /panicked|unreachable/.test(c)) && Date.now() - t0 > 12000) break; // iphey's WASM died: no result will come
}
const panic = cons.some(c => /panicked|unreachable/.test(c));
await sleep(2500); // let the tiles settle
text = String(await ev('document.body ? document.body.innerText : ""'));
const top = text.split('Any Check You Need')[0];
const shot = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }, sid);
writeFileSync(OUT + '/iphey.png', Buffer.from(shot.result.data, 'base64'));
writeFileSync(OUT + '/iphey.txt', text);
// open the Browser tile: its detail says why iphey rates the browser the way it does
let detail = '';
if (ready) {
  const pos = await ev(`(() => { const els = [...document.querySelectorAll('*')].filter(e => e.children.length === 0 && e.textContent.trim() === 'BROWSER' || e.children.length === 0 && /^click here to see$/i.test(e.textContent.trim()));
    if (!els.length) return null; const r = els[els.length - 1].getBoundingClientRect(); return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()`);
  if (pos && !String(pos).startsWith('ERR')) {
    const { x, y } = JSON.parse(pos);
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y }, sid);
    for (const type of ['mousePressed', 'mouseReleased']) await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 }, sid);
    await sleep(2000);
    const t2 = String(await ev('document.body ? document.body.innerText : ""'));
    const before = new Set(text.split('\n').map(s => s.trim()).filter(Boolean));
    detail = t2.split('\n').map(s => s.trim()).filter(s => s && !before.has(s)).join(' | ').slice(0, 1500);
    const s2 = await send('Page.captureScreenshot', { format: 'png' }, sid);
    writeFileSync(OUT + '/iphey-browser.png', Buffer.from(s2.result.data, 'base64'));
    writeFileSync(OUT + '/iphey-browser.txt', t2);
  }
}
const pending = [...reqs.values()].filter(r => !r.done && !r.status).map(r => r.url);
const bad = [...reqs.values()].filter(r => r.status >= 400).map(r => r.status + ' ' + r.url);
console.log(JSON.stringify({ ready, panic, detail, waited_s: Math.round((Date.now() - t0) / 1000), requests: reqs.size, pending: pending.slice(0, 15), http_errors: bad.slice(0, 15), failed: fails.slice(0, 15), console: cons.slice(0, 15), top: top.slice(0, 1800) }));
process.exit(0);
