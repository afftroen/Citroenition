// node probe.mjs <browser-ws-url> <echo-url> [fp-page-url]
// Opens <echo-url> (through whatever proxy the profile has) and a second tab on [fp-page-url] (default about:blank;
// pass a http://127.0.0.1 page to get a secure context, which navigator.userAgentData / deviceMemory require).
// Prints one JSON line: { echo: <what the origin saw: JSON or raw text>, fp: <fingerprint values read inside the page> }
const [, , WS, ECHO, PAGE = 'about:blank'] = process.argv;
const ws = new WebSocket(WS);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('cannot open ' + WS)); });
let id = 0; const pend = new Map();
ws.onmessage = e => { const d = JSON.parse(e.data); if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); } };
const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const tab = async url => {
  const { targetId } = (await send('Target.createTarget', { url })).result;
  return (await send('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
};
const ev = async (sid, expression) => {
  const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sid);
  return r.result?.result?.value ?? ('ERR ' + (r.result?.exceptionDetails?.exception?.description ?? r.error?.message ?? 'unknown'));
};
setTimeout(() => { console.error('probe timeout'); process.exit(3); }, 80000);

const out = {};
const sEcho = await tab(ECHO);
let body = '';
for (let i = 0; i < 60; i++) {
  body = await ev(sEcho, 'document.body ? document.body.innerText : ""');
  if (body && !body.startsWith('ERR')) break;
  await sleep(250);
}
try { out.echo = JSON.parse(body); } catch { out.echo = String(body).slice(0, 300); }

const E = {
  ua: 'navigator.userAgent', platform: 'navigator.platform', webdriver: 'String(navigator.webdriver)',
  lang: 'navigator.language', langs: 'JSON.stringify(navigator.languages)',
  tz: 'Intl.DateTimeFormat().resolvedOptions().timeZone', screen: '[screen.width, screen.height].join("x")',
  hc: 'navigator.hardwareConcurrency', mem: 'navigator.deviceMemory',
  secure: 'String(window.isSecureContext)',
  uad: 'navigator.userAgentData ? JSON.stringify({platform: navigator.userAgentData.platform, brands: navigator.userAgentData.brands.map(b => b.brand + " " + b.version)}) : "n/a"',
  canvas: `(() => { const c = document.createElement('canvas'); c.width = 200; c.height = 40; const x = c.getContext('2d');
    x.textBaseline = 'top'; x.font = '16px Arial'; x.fillStyle = '#f60'; x.fillRect(10, 5, 100, 25); x.fillStyle = '#069';
    x.fillText('citroenition fp', 2, 8); const s = c.toDataURL(); let h = 5381;
    for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0; return String(h >>> 0); })()`,
  webgl: `(() => { const g = document.createElement('canvas').getContext('webgl'); if (!g) return 'no webgl';
    const e = g.getExtension('WEBGL_debug_renderer_info');
    return e ? g.getParameter(e.UNMASKED_VENDOR_WEBGL) + ' | ' + g.getParameter(e.UNMASKED_RENDERER_WEBGL) : 'no debug info'; })()`,
  rect: `(() => { const d = document.createElement('div'); d.style.cssText = 'position:absolute;left:10.3px;top:20.7px;width:33.3px;height:17.1px';
    document.body.appendChild(d); const r = d.getBoundingClientRect(); return [r.x, r.y, r.width, r.height].join(','); })()`,
  rect2: `(() => { const s = document.createElement('span'); s.style.cssText = 'position:absolute;left:5.5px;top:7.25px;font:14.5px Arial';
    s.textContent = 'citroenition clientrects probe 0123456789'; document.body.appendChild(s);
    const a = s.getBoundingClientRect(); const rg = document.createRange(); rg.selectNodeContents(s);
    const b = rg.getBoundingClientRect(); const c = rg.getClientRects()[0]; const d = s.getClientRects()[0];
    return [a.width, a.height, b.width, b.x, c.width, c.height, d.width].join(','); })()`,
  rect3: `(() => { const N = 'http://www.w3.org/2000/svg'; const sv = document.createElementNS(N, 'svg'); sv.setAttribute('width', '200');
    sv.setAttribute('height', '100'); const t = document.createElementNS(N, 'text'); t.setAttribute('x', '10.3'); t.setAttribute('y', '30.7');
    t.setAttribute('font-size', '17.3'); t.textContent = 'citroenition svg'; sv.appendChild(t); document.body.appendChild(sv);
    const bb = t.getBBox(); const e = t.getExtentOfChar(0); return [bb.x, bb.y, bb.width, bb.height, e.width].join(','); })()`,
  audio: `(async () => { const c = new OfflineAudioContext(1, 5000, 44100); const o = c.createOscillator(); o.type = 'triangle';
    o.frequency.value = 1e4; const k = c.createDynamicsCompressor(); o.connect(k); k.connect(c.destination); o.start(0);
    const d = (await c.startRendering()).getChannelData(0); let s = 0; for (let i = 4500; i < 5000; i++) s += Math.abs(d[i]); return String(s); })()`,
};
const sBlank = await tab(PAGE);
await sleep(300);
out.fp = {};
for (const [k, e] of Object.entries(E)) out.fp[k] = await ev(sBlank, e);
console.log(JSON.stringify(out));
process.exit(0);
