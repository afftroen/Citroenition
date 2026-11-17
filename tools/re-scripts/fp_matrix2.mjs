// node fp_matrix2.mjs [case ...] : one SunBrowser launch per case; prints only the fingerprint keys that differ from `base`.
import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs'; import os from 'node:os'; import path from 'node:path';
const KERNEL = process.env.KERNEL || ""; // override with KERNEL=/path/to/SunBrowser
const STD = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const SHF = 'hTy1bfRJz4nLPcBCO7WtmNIaGvVeul5Zo8kq32UxrYw_-0gsjp96SDFXQiEMKdHA';
const enc = s => Buffer.from(s).toString('base64').split('').map(c => { const i = STD.indexOf(c); return i < 0 ? c : SHF[i]; }).join('');
const sleep = ms => new Promise(r => setTimeout(r, ms));
const ST = Math.floor(Date.now() / 1000);
const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36';
const UAM = { platform: 'Windows', platformVersion: '10.0.0', architecture: 'x86', model: '', mobile: false, bitness: '64', wow64: false };
const GLJSON = { UNMASKED_VENDOR_WEBGL: 'Google Inc. (NVIDIA)', UNMASKED_RENDERER_WEBGL: 'ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)', SUPPORTED_EXTENSIONS: [] };
const CASES = {
  base: {},
  'inl-lang': { inline: { Langs: 'fr-FR,fr', AcceptLang: 'fr-FR,fr;q=0.9' } },
  'stat-lang': { static: { Langs: 'fr-FR,fr', AcceptLang: 'fr-FR,fr;q=0.9' } },
  'stat-Langs-only': { static: { Langs: 'fr-FR,fr' } },
  'stat-Accept-only': { static: { AcceptLang: 'fr-FR,fr;q=0.9' } },
  'dyn-tz': { dynamic: { TimeZone: 'Pacific/Auckland' } },
  'flag-lang': { flags: ['--lang=fr-FR'] },
  'flag-Langs': { flags: ['--Langs=fr-FR,fr', '--AcceptLang=fr-FR,fr;q=0.9', '--TimeZone=Pacific/Auckland'] },
  'ua-flag+static': { flags: ['--user-agent=' + UA], inline: { Platform: 'Win32' }, static: { UserAgentMetadata: UAM } },
  'ua-static-only': { static: { UserAgentMetadata: UAM } },
  'mem-dnt': { inline: { DeviceMemory: 4, HardwareConcurrency: 6, EnableDoNotTrack: true } },
  'canvas-A': { inline: { CanvasMark: 11111, StartTime: ST } },
  'canvas-B': { inline: { CanvasMark: 22222, StartTime: ST } },
  'canvas-noStart': { inline: { CanvasMark: 11111 } },
  'audio-A': { inline: { AudioFp: 111 } },
  'audio-B': { inline: { AudioFp: 222 } },
  'cr-1234': { inline: { ClientRectFp: 1234, StartTime: ST } },
  'cr-9999': { inline: { ClientRectFp: -9999, StartTime: ST } },
  'webgl-file': { webglfp: GLJSON, gpu: true },
  'webgl-mark': { webglfp: GLJSON, inline: { WebGLMark: 777, StartTime: ST }, gpu: true },
  'webgl-enc': { webglfp: GLJSON, webglEnc: true, inline: { WebGLMark: 777, StartTime: ST }, gpu: true },
  'webgl-inline': { inline: { WebGLFP: JSON.stringify(GLJSON), WebGLMark: 777, StartTime: ST }, gpu: true },
  'hdr-lang-full': { static: { Langs: 'fr-FR,fr', AcceptLang: 'fr-FR,fr;q=0.9' }, inline: { AcceptLang: 'fr-FR,fr;q=0.9', Langs: 'fr-FR,fr' } },
  'hdr-ua-platform': { flags: ['--user-agent=' + UA], inline: { Platform: 'Win32' } },
  'geo': { inline: { GeolocationSetting: 'allow', Geoposition: '48.8566,2.3522,100' }, dynamic: { Geoposition: '48.8566,2.3522,100' }, geo: true },
};
let lastHdr = {};
const srv = http.createServer((q, s) => { if (q.url === '/') lastHdr = q.headers; s.setHeader('content-type', 'text/html'); s.end('<!doctype html><title>t</title><body><span id="t" style="font:15.3px Arial">hello world fingerprint</span>'); });
await new Promise(r => srv.listen(0, '127.0.0.1', r));
const URL = `http://127.0.0.1:${srv.address().port}/`;
const MEASURE = (geo) => `(async () => { if (location.href !== ${JSON.stringify(URL)} || document.readyState !== 'complete') return 'notready'; const o = {};
  o.sec = isSecureContext; o.ua = navigator.userAgent; o.lang = navigator.language; o.langs = navigator.languages.join(',');
  o.plat = navigator.platform; o.hc = navigator.hardwareConcurrency; o.mem = navigator.deviceMemory; o.dnt = navigator.doNotTrack;
  o.scr = screen.width + 'x' + screen.height; o.tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  if (navigator.userAgentData) { const u = navigator.userAgentData; o.uad = u.platform + '/' + u.mobile;
    try { const v = await u.getHighEntropyValues(['platformVersion','architecture','bitness','model']); o.hev = [v.platformVersion, v.architecture, v.bitness, JSON.stringify(v.model)].join('|'); } catch (e) { o.hev = String(e); } }
  { const c = document.createElement('canvas'); c.width = 80; c.height = 30; const x = c.getContext('2d'); x.fillStyle = '#f60'; x.fillRect(0, 0, 80, 30); x.fillStyle = '#069'; x.font = '14px Arial'; x.fillText('citro,fp', 2, 15); const u = c.toDataURL(); o.cv = u.length + ':' + u.slice(-40, -20); }
  { const c = new OfflineAudioContext(1, 4400, 44100); const os = c.createOscillator(); os.type = 'triangle'; os.frequency.value = 1e4; const k = c.createDynamicsCompressor(); os.connect(k); k.connect(c.destination); os.start(0); const b = await c.startRendering(); let s = 0; const d = b.getChannelData(0); for (let i = 0; i < d.length; i++) s += Math.abs(d[i]); o.au = s.toFixed(6); }
  { const sp = document.getElementById('t'); const r1 = sp.getBoundingClientRect(); const rr = document.createRange(); rr.selectNodeContents(sp); const r2 = rr.getBoundingClientRect(); const r3 = sp.getClientRects()[0]; const dv = document.createElement('div'); dv.style.cssText = 'position:absolute;left:10.3px;top:20.7px;width:33.3px;height:17.1px'; document.body.appendChild(dv); const r4 = dv.getBoundingClientRect(); o.cr = [r1.width, r2.width, r3.width, r4.x, r4.width].map(v => v.toFixed(5)).join(','); }
  try { const gl = document.createElement('canvas').getContext('webgl'); if (gl) { const e = gl.getExtension('WEBGL_debug_renderer_info'); o.gl = e ? gl.getParameter(e.UNMASKED_VENDOR_WEBGL) + ' / ' + gl.getParameter(e.UNMASKED_RENDERER_WEBGL) : 'noext'; } else o.gl = 'none'; } catch (e) { o.gl = 'err ' + e; }
  ${geo ? `o.geo = await new Promise(res => { const t = setTimeout(() => res('timeout'), 4000); navigator.geolocation.getCurrentPosition(p => { clearTimeout(t); res(p.coords.latitude.toFixed(4) + ',' + p.coords.longitude.toFixed(4) + ',' + p.coords.accuracy); }, e => { clearTimeout(t); res('err:' + e.message.slice(0, 40)); }); });` : ''}
  return JSON.stringify(o); })()`;

async function run(name, c) {
  const ud = fs.mkdtempSync(path.join(os.tmpdir(), 'fpm-'));
  const inline = { ...(c.inline || {}) };
  const put = (n, obj) => { const p = path.join(ud, n); fs.writeFileSync(p, enc(JSON.stringify(obj))); return p; };
  if (c.static) inline.StaticConfig = put('static.cfg', c.static);
  if (c.dynamic) inline.DynamicConfig = put('dynamic.cfg', c.dynamic);
  if (c.webglfp) { const p = path.join(ud, 'webgl.json'); fs.writeFileSync(p, c.webglEnc ? enc(JSON.stringify(c.webglfp)) : JSON.stringify(c.webglfp)); inline.WebGLFP = p; }
  const args = ['--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--protected-disable-safe-open', '--remote-debugging-port=0', `--user-data-dir=${ud}`, ...(c.gpu ? ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] : ['--disable-gpu']), ...(c.flags || [])];
  if (Object.keys(inline).length) args.push('--extended-parameters=' + enc(JSON.stringify(inline)));
  args.push('about:blank');
  const p = spawn(KERNEL, args, { detached: true, stdio: ['ignore', 'ignore', 'pipe'] });
  let err = ''; p.stderr.on('data', d => { err += d; });
  const kill = () => { try { process.kill(-p.pid, 'SIGKILL'); } catch {} try { fs.rmSync(ud, { recursive: true, force: true }); } catch {} };
  try {
    let m; for (let i = 0; i < 80 && !m; i++) { m = /DevTools listening on (ws:\/\/\S+)/.exec(err); if (!m) await sleep(250); }
    if (!m) return { _err: `no devtools (exit=${p.exitCode}) ${err.slice(0, 160).replace(/\n/g, ' ')}` };
    const ws = new WebSocket(m[1]); await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    let id = 0; const pend = new Map();
    ws.onmessage = e => { const d = JSON.parse(e.data); if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); } };
    const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
    if (c.geo) await send('Browser.grantPermissions', { permissions: ['geolocation'], origin: URL.replace(/\/$/, '') });
    const { targetId } = (await send('Target.createTarget', { url: URL })).result;
    const { sessionId } = (await send('Target.attachToTarget', { targetId, flatten: true })).result;
    let last = '';
    for (let t = 0; t < 30; t++) {
      const r = await send('Runtime.evaluate', { expression: MEASURE(!!c.geo), returnByValue: true, awaitPromise: true }, sessionId);
      const v = r.result?.result?.value;
      if (typeof v === 'string' && v !== 'notready') { const o = JSON.parse(v); o.h_al = lastHdr['accept-language']; o.h_ua = (lastHdr['user-agent'] || '').slice(0, 60); o.h_plat = lastHdr['sec-ch-ua-platform']; return o; }
      last = typeof v === 'string' ? v : JSON.stringify(r.result?.exceptionDetails?.exception?.description ?? r.error ?? r.result).slice(0, 120);
      await sleep(300);
    }
    return { _err: 'gave up: ' + last };
  } finally { kill(); }
}
const want = process.argv.slice(2); const names = ['base', ...(want.length ? want : Object.keys(CASES)).filter(n => n !== 'base')];
let base = null;
for (const n of names) {
  let o; try { o = await Promise.race([run(n, CASES[n]), sleep(30000).then(() => ({ _err: 'TIMEOUT' }))]); } catch (e) { o = { _err: String(e) }; }
  if (n === 'base') { base = o; console.log('base: ' + JSON.stringify(o)); continue; }
  if (o._err) { console.log(`${n}: ERR ${o._err}`); continue; }
  const d = {}; for (const k of Object.keys(o)) if (JSON.stringify(o[k]) !== JSON.stringify(base?.[k])) d[k] = o[k];
  console.log(`${n}: ` + (Object.keys(d).length ? JSON.stringify(d) : '(no change vs base)'));
}
srv.close(); process.exit(0);
