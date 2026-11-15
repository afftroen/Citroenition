#!/usr/bin/env python3
"""End-to-end proof against a REAL SunBrowser kernel.

    cargo build -p citro && python3 -I tests/e2e/run.py          # needs an AdsPower install (its kernel) and node >= 22

What it does: starts a local HTTP proxy that demands credentials, creates two profiles through `citro`, launches the
real kernel headless, and reads (a) what the page itself reports over CDP and (b) what the origin saw on the wire.
Then it drives the AdsPower-compatible Local API (`citro serve`) against the same store.

Env: CITRO (binary, default target/debug/citro), NODE (node binary, default: first on PATH).
Exit status is 0 only if every check passed. Everything runs in a throw-away CITRO_HOME.
"""
import base64
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
CITRO = os.environ.get("CITRO") or str(HERE.parents[1] / "target" / "debug" / "citro")
NODE = os.environ.get("NODE") or shutil.which("node")
PROBE = str(HERE / "probe.mjs")
USER, PASS = "proxyuser", "s3cr3t!pw"
WANT = "Basic " + base64.b64encode(f"{USER}:{PASS}".encode()).decode()
hits = {"407": 0, "200": 0}


class Proxy(BaseHTTPRequestHandler):
    """A forward proxy that only answers requests carrying the right Proxy-Authorization, and echoes what it saw."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_GET(self):
        if self.path.startswith("/"):  # a direct (non-proxied) request: the loopback page the probe reads the fingerprint on
            body = b"<!doctype html><title>fp</title><body></body>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.headers.get("Proxy-Authorization") != WANT:
            hits["407"] += 1
            self.send_response(407)
            self.send_header("Proxy-Authenticate", 'Basic realm="citro"')
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        hits["200"] += 1
        body = json.dumps({
            "url": self.path,
            "ua": self.headers.get("User-Agent"),
            "accept_language": self.headers.get("Accept-Language"),
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


tmp = tempfile.mkdtemp(prefix="citro-e2e-", dir=os.environ.get("TMPDIR"))
env = dict(os.environ, CITRO_HOME=tmp)
srv = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
threading.Thread(target=srv.serve_forever, daemon=True).start()
PROXY = f"http://{USER}:{PASS}@127.0.0.1:{srv.server_address[1]}"
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"   [{detail}]" if detail else ""))


def note(text):
    print("NOTE " + text)


def citro(*a):
    r = subprocess.run([CITRO, *a], env=env, capture_output=True, text=True, timeout=150)
    if r.returncode:
        raise SystemExit(f"citro {' '.join(a[:2])} -> rc={r.returncode}: {r.stderr.strip()[:600]}")
    return r


def probe(ws):
    page = f"http://127.0.0.1:{srv.server_address[1]}/blank"
    r = subprocess.run([NODE, PROBE, ws, "http://echo.test/", page], capture_output=True, text=True, timeout=100)
    if r.returncode:
        raise SystemExit("probe failed: " + (r.stderr or r.stdout)[-800:])
    return json.loads(r.stdout)


def inline_params(profile):
    """The `--extended-parameters` object a launch would pass to the kernel (nothing is started)."""
    d = json.loads(citro("open", profile, "--headless", "--dry-run").stdout)
    return next(a["--extended-parameters"] for a in d["args"] if isinstance(a, dict) and "--extended-parameters" in a)


def api(port, method, path, key, body=None):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=120) as resp:
        return json.loads(resp.read())


def alive(pid):
    return subprocess.run(["ps", "-p", str(pid)], capture_output=True).returncode == 0


A = B = server = None
try:
    if not NODE:
        raise SystemExit("node not found: set NODE=/path/to/node")
    print(citro("kernel").stdout.strip())
    A = citro("new", "fr-shopper", "--country", "fr", "--lang", "fr-FR", "--lang", "fr", "--timezone", "Europe/Paris",
              "--screen", "1920x1080", "--proxy", PROXY).stdout.strip()
    B = citro("new", "de-buyer", "--country", "de", "--proxy", PROXY).stdout.strip()
    print("profiles:", A, B)

    # ---- profile A through the CLI -------------------------------------------------------------
    pa_in, pb_in = inline_params(A), inline_params(B)
    oa = json.loads(citro("open", A, "--headless", "--json").stdout)
    pa = probe(oa["ws"]["puppeteer"])
    fa, ea = pa["fp"], pa["echo"]
    ver = json.load(urllib.request.urlopen(f"http://127.0.0.1:{oa['debug_port']}/json/version", timeout=10))
    check("the kernel really is Chrome 157 (CDP /json/version)", ver["Browser"].startswith("Chrome/157"), ver["Browser"])
    check("UA is Chrome/157", "Chrome/157" in fa["ua"], fa["ua"])
    check("navigator.platform = Win32", fa["platform"] == "Win32", fa["platform"])
    uad = fa["uad"]
    check("userAgentData.platform = Windows", uad.startswith("{") and json.loads(uad)["platform"] == "Windows",
          f"secure={fa['secure']} {uad[:90]}")
    check("navigator.webdriver is false", fa["webdriver"] in ("false", "undefined"), fa["webdriver"])
    check("languages = fr-FR,fr", json.loads(fa["langs"]) == ["fr-FR", "fr"], fa["langs"])
    check("time zone = Europe/Paris", fa["tz"] == "Europe/Paris", fa["tz"])
    check("screen = 1920x1080", fa["screen"] == "1920x1080", fa["screen"])
    ok_echo = isinstance(ea, dict) and ea.get("url", "").startswith("http://echo.test")
    check("proxy credentials accepted (origin reached through the proxy)", ok_echo, str(ea)[:120])
    check("wire User-Agent == JS userAgent", ok_echo and ea.get("ua") == fa["ua"])
    check("wire Accept-Language = fr-FR,fr;q=0.9", ok_echo and ea.get("accept_language") == "fr-FR,fr;q=0.9",
          str(ea.get("accept_language") if ok_echo else ea))
    cmd = subprocess.run(["ps", "-o", "args=", "-p", str(oa["pid"])], capture_output=True, text=True).stdout
    check("proxy password is NOT on the kernel command line", PASS not in cmd and "s3cr3t" not in cmd)
    mode = oct(os.stat(f"{tmp}/store.json").st_mode & 0o777)
    check("store.json is 0600", mode == "0o600", mode)
    check("citro ps lists the browser", json.loads(citro("ps", "--json").stdout)[0]["profile_id"] == A)
    check("opening again is idempotent (same pid)",
          json.loads(citro("open", A, "--headless", "--json").stdout)["pid"] == oa["pid"])
    citro("close", A)
    time.sleep(0.5)
    check("citro close stops it", not alive(oa["pid"]) and citro("ps", "--json").stdout.strip() == "[]")

    # ---- same profile again: identity must be stable ---------------------------------------
    oa2 = json.loads(citro("open", A, "--headless", "--json").stdout)
    fa2 = probe(oa2["ws"]["puppeteer"])["fp"]
    for k in ("canvas", "webgl", "audio", "rect", "rect2", "rect3", "ua", "langs", "tz"):
        check(f"relaunch keeps {k}", fa2[k] == fa[k], fa[k][:60])
    citro("close", A)

    # ---- profile B: defaults come from the country, fingerprint differs from A -------------
    ob = json.loads(citro("open", B, "--headless", "--json").stdout)
    fb = probe(ob["ws"]["puppeteer"])["fp"]
    check("B languages from country de", json.loads(fb["langs"]) == ["de-DE", "de"], fb["langs"])
    check("B time zone from country de", fb["tz"] == "Europe/Berlin", fb["tz"])
    check("B canvas differs from A", fb["canvas"] != fa["canvas"], f"{fa['canvas']} vs {fb['canvas']}")
    check("B audio differs from A", fb["audio"] != fa["audio"])
    check("ClientRectFp is emitted and differs per profile", pa_in["ClientRectFp"] != pb_in["ClientRectFp"],
          f"{pa_in['ClientRectFp']} vs {pb_in['ClientRectFp']}")
    ra, rb = [(f["rect"], f["rect2"], f["rect3"]) for f in (fa, fb)]
    note("ClientRectFp " + ("changes layout geometry on this kernel" if ra != rb else
         "is passed through but this headless kernel shows no layout difference (div, text+Range, SVG getBBox)"))
    note(f"WebGL (SwiftShader in headless): A={fa['webgl']} | B={fb['webgl']}")

    # ---- the AdsPower Local API, same store, real kernel -------------------------------------
    key, port = "e2e-key", free_port()
    log = open(f"{tmp}/serve.log", "w")
    server = subprocess.Popen([CITRO, "serve", "--bind", f"127.0.0.1:{port}", "--api-key", key], env=env,
                              stdout=log, stderr=log)
    for _ in range(50):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/status", timeout=1)
            break
        except Exception:
            time.sleep(0.2)
    st = api(port, "GET", "/status", key)
    check("GET /status", st.get("code") == 0, str(st))
    try:
        urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{port}/api/v1/user/list"), timeout=5)
        check("missing API key is rejected", False)
    except urllib.error.HTTPError as e:
        check("missing API key is rejected (401)", e.code == 401)
    lst = api(port, "POST", "/api/v2/browser-profile/list", key, {"limit": 10})
    check("v2 list sees both profiles and leaks no proxy password",
          lst["data"]["total"] == 2 and PASS not in json.dumps(lst), f"total={lst['data']['total']}")
    sb = api(port, "POST", "/api/v2/browser-profile/start", key, {"profile_id": B, "headless": "1"})
    check("API start returns the CLI-opened browser (shared run records)",
          sb["code"] == 0 and sb["data"]["debug_port"] == str(ob["debug_port"]), str(sb)[:160])
    sa = api(port, "POST", "/api/v2/browser-profile/start", key, {"profile_id": A, "headless": "1"})
    check("API start opens A", sa["code"] == 0, str(sa)[:160])
    fapi = probe(sa["data"]["ws"]["puppeteer"])["fp"]
    check("A through the API has the same fingerprint as through the CLI",
          fapi["canvas"] == fa["canvas"] and fapi["tz"] == fa["tz"])
    act = api(port, "GET", "/api/v1/browser/local-active", key)
    check("local-active lists 2 browsers", len(act["data"]["list"]) == 2)
    for pid in (A, B):
        s = api(port, "POST", "/api/v2/browser-profile/stop", key, {"profile_id": pid})
        check(f"API stop {pid}", s["code"] == 0)
    time.sleep(0.5)
    check("nothing left running", citro("ps", "--json").stdout.strip() == "[]")
    print(f"proxy saw {hits['407']} auth challenge(s) and {hits['200']} authenticated request(s)")
except BaseException:  # incl. SystemExit from citro()/probe(): an aborted run must count as a failure
    traceback.print_exc()
    results.append(False)
finally:
    left = [p for p in (A, B) if p]
    if left:
        subprocess.run([CITRO, "close", *left], env=env, capture_output=True)
    if server:
        server.terminate()
    srv.shutdown()
    stray = subprocess.run(["pgrep", "-f", f"--user-data-dir={tmp}"], capture_output=True, text=True).stdout.split()
    shutil.rmtree(tmp, ignore_errors=True)
    check("no kernel process left behind", not stray, ",".join(stray))
print(f"\n{sum(results)}/{len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)
