#!/usr/bin/env bash
# Real-kernel smoke test: with the programs of a release archive, install the Clearcote kernel for THIS machine, open
# a profile, ask the running browser who it is over DevTools, check that `citro ps` / `citro close` see that very
# process, and close it. It needs the network (about 120 MB on Linux, 220 MB on macOS) and a throw-away home.
#
#   scripts/smoke-kernel.sh <directory that holds `citro`> [headless|window]
#
# `window` opens the browser with a window: it needs a desktop session. Set GITHUB_TOKEN to avoid GitHub's low
# rate limit for anonymous API calls (the release workflow does). Exit status 0 only when every step held.
set -euo pipefail

dir=${1:?usage: smoke-kernel.sh <directory that holds citro> [headless|window]}
mode=${2:-headless}
citro="$dir/citro"
[ -x "$citro" ] || { echo "no executable $citro" >&2; exit 1; }
case "$mode" in headless | window) ;; *) echo "mode is headless or window, not $mode" >&2; exit 1 ;; esac

case "$(uname -s)" in
    Darwin) want_os="Macintosh" ;;
    *) want_os="X11; Linux" ;;
esac

CITRO_HOME=$(mktemp -d)
export CITRO_HOME
pid=""
cleanup() {
    "$citro" close 1 >/dev/null 2>&1 || true
    rm -rf "$CITRO_HOME"
}
trap cleanup EXIT

json_field() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

echo "=== citro --version"
"$citro" --version
echo "=== citro kernel install"
"$citro" kernel install
echo "=== citro kernel status"
"$citro" kernel status

echo "=== citro new"
"$citro" new smoke --proxy none --country fr
if [ "$mode" = headless ]; then
    echo "=== citro open 1 --headless"
    "$citro" open 1 --headless
else
    echo "=== citro open 1 (a window)"
    "$citro" open 1
fi

port=$("$citro" ls --json | json_field '[p for p in d if p["running"]][0]["debug_port"]')
echo "=== the browser answers on 127.0.0.1:$port"
version=$(curl -fsS --max-time 20 "http://127.0.0.1:$port/json/version")
echo "$version"
ua=$(printf '%s' "$version" | json_field 'd["User-Agent"]')
case "$ua" in *"$want_os"*) echo "user agent names the host OS ($want_os): ok" ;; *) echo "FAIL: user agent does not contain '$want_os': $ua" >&2; exit 1 ;; esac
case "$ua" in *Chrome/*) ;; *) echo "FAIL: not a Chrome user agent: $ua" >&2; exit 1 ;; esac
echo "--- pages"
curl -fsS --max-time 20 "http://127.0.0.1:$port/json/list" | json_field '"\n".join(t["type"] + "  " + t["url"] for t in d)'

echo "=== citro ps sees that process"
"$citro" ps
pid=$("$citro" ps --json | json_field 'd[0]["pid"]')
kill -0 "$pid"
echo "pid $pid is alive"

echo "=== citro close 1"
"$citro" close 1
for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
done
if kill -0 "$pid" 2>/dev/null; then
    echo "FAIL: pid $pid is still alive after close" >&2
    exit 1
fi
left=$("$citro" ps --json | json_field 'len(d)')
[ "$left" = 0 ] || { echo "FAIL: citro ps still lists $left browser(s)" >&2; exit 1; }
echo "closed: pid $pid is gone and ps is empty"
echo "SMOKE-OK ($mode, $(uname -sm))"
