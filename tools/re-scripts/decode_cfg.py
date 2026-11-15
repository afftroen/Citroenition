import os, re, json, base64, glob, hashlib
STD = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
SHF = "hTy1bfRJz4nLPcBCO7WtmNIaGvVeul5Zo8kq32UxrYw_-0gsjp96SDFXQiEMKdHA"
dec = str.maketrans(SHF, STD)
def decode(raw: bytes) -> bytes: return base64.b64decode(raw.decode().translate(dec))
sens = re.compile(r'(?i)(pass|account|secret|token|cookie|key|proxyuser|address)')
def scrub(v, k=""):
    if isinstance(v, dict): return {a: scrub(b, a) for a, b in v.items()}
    if isinstance(v, list):
        if k == "ProxyChain": return [{kk: ("<redacted>" if kk in ("host", "account", "password", "port", "user") else vv) for kk, vv in x.items()} if isinstance(x, dict) else x for x in v]
        return [scrub(x, k) for x in v][:4]
    if sens.search(k) and isinstance(v, str) and v: return "<redacted:%d>" % len(v)
    return v if not isinstance(v, str) or len(v) < 90 else v[:90] + "…(%d)" % len(v)
root = os.path.expanduser("~/.adspowerCli/source/cache")
for d in sorted(glob.glob(root + "/*/")):
    print("\n#####", os.path.basename(d.rstrip('/')))
    for f in sorted(os.listdir(d)):
        p = os.path.join(d, f)
        if re.fullmatch(r'[0-9a-f]{32}', f) and os.path.isfile(p) and os.path.getsize(p) > 0:
            raw = open(p, 'rb').read()
            try: j = json.loads(decode(raw))
            except Exception as e:
                try: j = json.loads(raw)
                except Exception: print(" ", f, len(raw), "not decodable:", repr(raw[:30])); continue
            if isinstance(j, dict): print(" ", f, "dict keys:", len(j)); print("    ", json.dumps(scrub(j), ensure_ascii=False)[:5200])
            else: print(" ", f, type(j).__name__, json.dumps(scrub(j), ensure_ascii=False)[:300])
