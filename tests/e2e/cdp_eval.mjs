// node cdp_eval.mjs <browser-ws-url> <url> <expr-json-array>
// Opens <url> in a new tab, evaluates every JS expression (awaited) and prints {expr: value} as one JSON object.
const [, , WS, URL_, EXPRS] = process.argv;
const ws = new WebSocket(WS);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('cannot open ws')); });
let id = 0; const pend = new Map();
ws.onmessage = e => { const d = JSON.parse(e.data); if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); } };
const send = (method, params = {}, sessionId) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
setTimeout(() => { console.error('cdp_eval timeout'); process.exit(3); }, 60000);
const { targetId } = (await send('Target.createTarget', { url: URL_ })).result;
const sid = (await send('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
await send('Runtime.enable', {}, sid);
await new Promise(r => setTimeout(r, 3500));
const out = {};
for (const ex of JSON.parse(EXPRS)) {
  const r = await send('Runtime.evaluate', { expression: ex, returnByValue: true, awaitPromise: true }, sid);
  out[ex] = r.result?.exceptionDetails ? 'ERR ' + (r.result.exceptionDetails.exception?.description ?? '').split('\n')[0] : r.result?.result?.value;
}
console.log(JSON.stringify(out));
process.exit(0);
