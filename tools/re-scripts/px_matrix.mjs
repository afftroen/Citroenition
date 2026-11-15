// node px_matrix.mjs : which channel makes SunBrowser authenticate to an HTTP proxy that demands Basic auth?
import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs'; import os from 'node:os'; import path from 'node:path';
const KERNEL = process.env.KERNEL || `${process.env.HOME}/.adspowerCli/chrome_157/SunBrowser`; // override with KERNEL=/path/to/SunBrowser
const STD = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const SHF = 'hTy1bfRJz4nLPcBCO7WtmNIaGvVeul5Zo8kq32UxrYw_-0gsjp96SDFXQiEMKdHA';
const enc = s => Buffer.from(s).toString('base64').split('').map(c => { const i = STD.indexOf(c); return i < 0 ? c : SHF[i]; }).join('');
const sleep = ms => new Promise(r => setTimeout(r, ms));
const GOOD = 'Basic ' + Buffer.from('citrou:citrop').toString('base64');
const CASES = {
  'none(no creds)': { flags: ['--proxy-server=http://127.0.0.1:__PORT__'] },
  'inline ProxyUser/Password': { flags: ['--proxy-server=http://127.0.0.1:__PORT__'], inline: { ProxyUser: 'citrou', ProxyPassword: 'citrop' } },
  'url creds in flag': { flags: ['--proxy-server=http://citrou:citrop@127.0.0.1:__PORT__'] },
  'static ProxyChain only': { static: { ProxyChain: [{ scheme: 'http', host: '127.0.0.1', port: '__PORT__', account: 'citrou', password: 'citrop' }] } },
  'flag + static ProxyChain': { flags: ['--proxy-server=http://127.0.0.1:__PORT__'], static: { ProxyChain: [{ scheme: 'http', host: '127.0.0.1', port: '__PORT__', account: 'citrou', password: 'citrop' }] } },
};
async function run(name, spec) {
  const log = { reqs: 0, authed: 0, c407: 0, hosts: new Set() };
  const px = http.createServer((req, res) => {
    log.reqs++; log.hosts.add((req.headers.host || '').slice(0, 30));
    if ((req.headers['proxy-authorization'] || '') === GOOD) { log.authed++; res.setHeader('content-type', 'text/html'); res.end('<!doctype html><body>via-proxy-ok'); }
    else { log.c407++; res.statusCode = 407; res.setHeader('Proxy-Authenticate', 'Basic realm="citro"'); res.end('auth required'); }
  });
  await new Promise(r => px.listen(0, '127.0.0.1', r));
  const c = JSON.parse(JSON.stringify(spec).replaceAll('__PORT__', String(px.address().port)));
  const ud = fs.mkdtempSync(path.join(os.tmpdir(), 'pxm-'));
  const inline = { ...(c.inline || {}) };
  if (c.static) { const p = path.join(ud, 'static.cfg'); fs.writeFileSync(p, enc(JSON.stringify(c.static))); inline.StaticConfig = p; }
  const args = ['--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu', '--protected-disable-safe-open', '--remote-debugging-port=0', `--user-data-dir=${ud}`, ...(c.flags || [])];
  if (Object.keys(inline).length) args.push('--extended-parameters=' + enc(JSON.stringify(inline)));
  args.push('about:blank');
  const p = spawn(KERNEL, args, { detached: true, stdio: ['ignore', 'ignore', 'pipe'] });
  let err = ''; p.stderr.on('data', d => { err += d; });
  let body = '?';
  try {
    let m; for (let i = 0; i < 80 && !m; i++) { m = /DevTools listening on (ws:\/\/\S+)/.exec(err); if (!m) await sleep(250); }
    if (!m) return `${name}: no devtools`;
    const ws = new WebSocket(m[1]); await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    let id = 0; const pend = new Map();
    ws.onmessage = e => { const d = JSON.parse(e.data); if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); } };
    const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
    const { targetId } = (await send('Target.createTarget', { url: 'http://citro-target.test/' })).result;
    const { sessionId } = (await send('Target.attachToTarget', { targetId, flatten: true })).result;
    for (let i = 0; i < 20; i++) { await sleep(300); const r = await send('Runtime.evaluate', { expression: 'document.body ? document.body.innerText.slice(0,40) + " | " + location.href.slice(0,40) : "nobody"', returnByValue: true }, sessionId); body = r.result?.result?.value ?? JSON.stringify(r.error ?? r.result?.exceptionDetails?.text); if (/via-proxy-ok/.test(body)) break; }
  } finally { try { process.kill(-p.pid, 'SIGKILL'); } catch {} try { fs.rmSync(ud, { recursive: true, force: true }); } catch {} px.close(); }
  return `${name}: proxyReqs=${log.reqs} authed=${log.authed} got407=${log.c407} page="${String(body).replace(/\n/g, ' ')}"`;
}
for (const [n, s] of Object.entries(CASES)) { try { console.log(await Promise.race([run(n, s), sleep(30000).then(() => `${n}: TIMEOUT`)])); } catch (e) { console.log(`${n}: ERR ${e}`); } }
process.exit(0);
