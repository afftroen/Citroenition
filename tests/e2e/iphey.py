#!/usr/bin/env python3
"""Real-world proof: `citro open` (Rust launcher + the open-source Clearcote kernel) on iphey, CreepJS and
BrowserScan, through a SOCKS5 proxy that needs a login.

  CITRO_TEST_PROXY='socks5://host:port:user:pass' python3 -I tests/e2e/iphey.py OUT_DIR \
      [--os linux|macos|windows] [--sites iphey,creepjs,browserscan] [--kernel-dir EXTRACTED_CLEARCOTE_DIR]

Without --kernel-dir the kernel comes from `citro kernel install` (download + signature + sha256 check).
Everything printed has the proxy login redacted. Exit 0 only if the browser's exit IP is the proxy's.
Env: CITRO (binary; default target dir of this checkout), NODE.
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("out")
ap.add_argument("--os", default="linux", dest="persona_os")
ap.add_argument("--sites", default="iphey,creepjs,browserscan")
ap.add_argument("--kernel-dir")
ap.add_argument("--display", default=":96")
ap.add_argument("--headless", action="store_true")
a = ap.parse_args()

HERE = Path(__file__).resolve().parent
CITRO = os.environ.get("CITRO") or str(HERE.parents[1] / "target" / "debug" / "citro")
NODE = os.environ.get("NODE") or shutil.which("node") or str(Path.home() / ".hermes/tools/node-26.12.1-linux-x64/bin/node")
raw = os.environ["CITRO_TEST_PROXY"].split("://", 1)[1]
PH, PP, PU, PW = raw.split(":", 3)
red = lambda s: str(s).replace(PW, "<redacted>").replace(PU, "<redacted>")
out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
home = tempfile.mkdtemp(prefix="citro-iphey-", dir=os.environ.get("TMPDIR"))
env = {k: v for k, v in os.environ.items() if k != "CITRO_TEST_PROXY"}
env.update(CITRO_HOME=home, DISPLAY=a.display)
procs, ok = [], False


def sh(*cmd, timeout=120, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, **kw)


def citro(*args, timeout=180):
    r = sh(CITRO, *args, timeout=timeout, env=env)
    if r.returncode:
        raise SystemExit(f"citro {' '.join(args[:2])} -> rc={r.returncode}: {red(r.stderr.strip())[:600]}")
    return r.stdout


try:
    if a.kernel_dir:  # an already-extracted kernel, linked into the layout `citro kernel install` produces
        ver = sh(str(Path(a.kernel_dir) / "chrome"), "--version").stdout.split()[-1]
        d = Path(home, "kernels"); d.mkdir(parents=True)
        (d / f"clearcote-{ver}").symlink_to(Path(a.kernel_dir).resolve())
    else:
        print(citro("kernel", "install", timeout=900).strip())
    print(citro("kernel").strip())

    g = sh("curl", "-s", "--max-time", "25", "-x", f"socks5h://{PH}:{PP}", "--proxy-user", f"{PU}:{PW}", "https://ipinfo.io/json")
    geo = json.loads(g.stdout) if g.stdout.strip().startswith("{") else {}
    cc = geo.get("country", "")
    print(f"proxy exit geo: country={cc} city={geo.get('city')} tz={geo.get('timezone')}")

    pid = citro("new", "e2e", "--os", a.persona_os, "--country", cc or "us", "--proxy", f"socks5://{PU}:{PW}@{PH}:{PP}").strip().splitlines()[0]
    if not a.headless:
        procs.append(subprocess.Popen(["Xvfb", a.display, "-screen", "0", "1920x1080x24", "-nolisten", "tcp"], stderr=subprocess.DEVNULL))
        sock = Path("/tmp/.X11-unix") / ("X" + a.display.lstrip(":"))
        for _ in range(60):
            if sock.exists():
                break
            time.sleep(0.1)
    opened = json.loads(citro("open", pid, "--json", *(["--headless"] if a.headless else [])))
    ws = opened["ws"]["puppeteer"]
    print(f"opened profile {pid}: pid={opened['pid']} port={opened['debug_port']}")
    cmd = Path(f"/proc/{opened['pid']}/cmdline").read_bytes().decode(errors="replace")
    print("proxy password on the kernel command line:", "yes" if PW in cmd else "no")

    p = sh(NODE, str(HERE / "probe.mjs"), ws, "https://ipinfo.io/json", timeout=120)
    try:
        j = json.loads(p.stdout); echo, fp = j["echo"], j["fp"]
    except Exception:
        echo, fp = {"raw": p.stdout[:200], "err": red(p.stderr)[:200]}, {}
    print("browser exit:", json.dumps({k: echo.get(k) for k in ("country", "city", "timezone", "org")} if isinstance(echo, dict) else echo))
    ok = isinstance(echo, dict) and echo.get("ip") == PH
    print("EXIT-IP == proxy:", ok)
    print("page-side:", json.dumps({k: fp.get(k) for k in ("ua", "platform", "lang", "langs", "tz", "webdriver", "webgl", "screen", "hc", "mem", "uad")}))

    sites = [s for s in a.sites.split(",") if s]
    if "iphey" in sites:
        q = sh(NODE, str(HERE / "iphey.mjs"), ws, str(out), timeout=175)
        try:
            r = json.loads(q.stdout)
            print("[iphey] ready=%s panic=%s waited=%ss" % (r["ready"], r.get("panic"), r["waited_s"]))
            print("[iphey] BROWSER detail:", r.get("detail") or "(none)")
            print("[iphey] tiles:", r["top"][:900].replace("\n", " | "))
        except Exception:
            print("[iphey] raw:", red(q.stdout)[:400], red(q.stderr)[:200])
    cfg = {
        "creepjs": ("https://abrahamjuliot.github.io/creepjs/", r"trust score|bot|lies", "100", "8"),
        "browserscan": ("https://www.browserscan.net/", r"authenticity|fingerprint|webrtc|ip address", "90", "8"),
    }
    for s in sites:
        if s in cfg:
            url, ready, mw, st = cfg[s]
            q = sh(NODE, str(HERE / "page_report.mjs"), ws, str(out), s, url, ready, mw, st, timeout=int(mw) + 80)
            try:
                r = json.loads(q.stdout)
                print("[%s] ready=%s waited=%ss chars=%s" % (s, r["ready"], r["waited_s"], r["chars"]))
                print("[%s] head: %s" % (s, red(r["head"])[:500].replace("\n", " | ")))
            except Exception:
                print("[%s] raw: %s %s" % (s, red(q.stdout)[:300], red(q.stderr)[:200]))

    citro("close", pid)
    shown = json.loads(citro("show", pid))
    closed = int(shown.get("last_close_time") or 0)
    print("last_close_time recorded:", closed > 0)
    ok = ok and closed > 0
    print("artifacts:", out)
finally:
    subprocess.run([CITRO, "close", "1"], env=env, capture_output=True)
    for p in reversed(procs):
        try:
            p.terminate(); p.wait(timeout=8)
        except Exception:
            p.kill()
    shutil.rmtree(home, ignore_errors=True)
sys.exit(0 if ok else 1)
