// node fp_probe.mjs <none|inline|file> : launch SunBrowser with params, read fingerprint values over CDP, kill it.
import { spawn } from 'node:child_process';
import fs from 'node:fs'; import os from 'node:os'; import path from 'node:path';
const KERNEL = process.env.KERNEL || ""; // override with KERNEL=/path/to/SunBrowser
const STD = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const SHF = 'hTy1bfRJz4nLPcBCO7WtmNIaGvVeul5Zo8kq32UxrYw_-0gsjp96SDFXQiEMKdHA';
const enc = s => Buffer.from(s).toString('base64').split('').map(c => { const i = STD.indexOf(c); return i < 0 ? c : SHF[i]; }).join('');
const mode = process.argv[2];
const ud = fs.mkdtempSync(path.join(os.tmpdir(), 'fpp-'));
const fp = { UserId: '380', Platform: 'Win32', HardwareConcurrency: 6, DeviceMemory: 4, ScreenSize: '1366,768', Langs: 'fr-FR,fr', AcceptLang: 'fr-FR,fr;q=0.9',
  TimeZone: 'Pacific/Auckland', CanvasMark: 424242, AudioFp: 777, ClientRectFp: 1234, WebGLMark: 99, StartTime: Math.floor(Date.now() / 1000) };
const args = ['--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu', '--protected-disable-safe-open', '--remote-debugging-port=0', `--user-data-dir=${ud}`];
if (mode === 'inline') args.push('--extended-parameters=' + enc(JSON.stringify(fp)));
if (mode === 'file') { const sc = path.join(ud, 'static.cfg'); fs.writeFileSync(sc, enc(JSON.stringify(fp))); args.push('--extended-parameters=' + enc(JSON.stringify({ UserId: '380', StaticConfig: sc }))); }
if (mode === 'full') {
  const sc = path.join(ud, 'static.cfg'), dc = path.join(ud, 'dyn.cfg');
  const stat = { ...fp, UserAgentMetadata: { platform: 'Windows', platformVersion: '10.0.0', architecture: 'x86', model: '', mobile: false, wow64: false, bitness: '64' } };
  fs.writeFileSync(sc, enc(JSON.stringify(stat)));
  fs.writeFileSync(dc, enc(JSON.stringify({ TimeZone: 'Pacific/Auckland', Geoposition: '-36.8485,174.7633,1000' })));
  args.push('--extended-parameters=' + enc(JSON.stringify({ ...fp, StaticConfig: sc, DynamicConfig: dc, Geoposition: '-36.8485,174.7633,1000' })));
}
args.push('about:blank');
const p = spawn(KERNEL, args, { detached: true, stdio: ['ignore', 'ignore', 'pipe'] });
let err = ''; p.stderr.on('data', d => { err += d; });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const done = c => { try { process.kill(-p.pid, 'SIGKILL'); } catch {} try { fs.rmSync(ud, { recursive: true, force: true }); } catch {} process.exit(c); };
setTimeout(() => { console.log(mode, 'TIMEOUT', err.slice(0, 300)); done(2); }, 30000);
let m; for (let i = 0; i < 80 && !m; i++) { m = /DevTools listening on (ws:\/\/\S+)/.exec(err); if (!m) await sleep(250); }
if (!m) { console.log(mode, 'no devtools line; exit=', p.exitCode, err.slice(0, 300)); done(1); }
const ws = new WebSocket(m[1]); await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let id = 0; const pend = new Map();
ws.onmessage = e => { const d = JSON.parse(e.data); if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); } };
const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
const { targetId } = (await send('Target.createTarget', { url: 'about:blank' })).result;
const { sessionId } = (await send('Target.attachToTarget', { targetId, flatten: true })).result;
const E = {
  secure: 'String(isSecureContext)', uadPlatform: 'navigator.userAgentData ? navigator.userAgentData.platform : "n/a"',
  hev: 'navigator.userAgentData ? navigator.userAgentData.getHighEntropyValues(["platformVersion","architecture","bitness","model"]).then(v=>JSON.stringify(v)) : "n/a"',
  platform: 'navigator.platform', hc: 'navigator.hardwareConcurrency', mem: 'navigator.deviceMemory', screen: '[screen.width, screen.height].join("x")',
  langs: 'JSON.stringify(navigator.languages)', tz: 'Intl.DateTimeFormat().resolvedOptions().timeZone',
  canvas: `(()=>{const c=document.createElement('canvas');c.width=80;c.height=30;const x=c.getContext('2d');x.fillStyle='#f60';x.fillRect(0,0,80,30);x.fillStyle='#069';x.font='14px Arial';x.fillText('citro,fp',2,15);return c.toDataURL().slice(-24)})()`,
  audio: `(async()=>{const c=new OfflineAudioContext(1,4400,44100);const o=c.createOscillator();o.type='triangle';o.frequency.value=1e4;const k=c.createDynamicsCompressor();o.connect(k);k.connect(c.destination);o.start(0);const b=await c.startRendering();let s=0;const d=b.getChannelData(0);for(let i=0;i<d.length;i++)s+=Math.abs(d[i]);return s.toFixed(6)})()`,
  rect: `(()=>{const d=document.createElement('div');d.style.cssText='position:absolute;left:10.3px;top:20.7px;width:33.3px;height:17.1px';document.body.appendChild(d);const r=d.getBoundingClientRect();return [r.x,r.y,r.width,r.height].map(v=>v.toFixed(5)).join(',')})()`,
};
const out = { mode };
for (const [k, e] of Object.entries(E)) { const r = await send('Runtime.evaluate', { expression: e, returnByValue: true, awaitPromise: true }, sessionId); out[k] = r.result?.result?.value ?? r.result?.exceptionDetails?.text ?? JSON.stringify(r.error); }
console.log(JSON.stringify(out)); done(0);
