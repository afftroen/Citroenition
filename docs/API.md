# The Local API

`citro serve` implements the **AdsPower Local API**. Scripts written for AdsPower (Selenium, Puppeteer, Playwright,
the vendor SDKs) point at it unchanged: same paths, same field names, same response envelope.

```sh
citro serve                                   # 127.0.0.1:50325, no key (loopback only)
citro serve --bind 0.0.0.0:50325 --api-key "$KEY"   # off loopback a key is mandatory
```

On start the server writes its address to `<home>/local_api` and removes the file on exit (AdsPower does the same).
Stop it with Ctrl-C or SIGTERM; browsers it opened are separate processes and keep running.

## Conventions

* **Envelope.** Every reply is HTTP 200 with `{"code": 0, "data": …, "msg": "success"}` or, on failure,
  `{"code": -1, "data": {}, "msg": "<reason>"}`. The only non-200 answers are **401** (missing or wrong API key) and
  **404** (unknown route).
* **Auth.** Without `--api-key` the server is open (loopback only). With one, every route except `/status` needs
  `Authorization: Bearer <key>`.
* **Lenient inputs.** Clients disagree about strings, numbers and booleans, so `"1"`, `1` and `true` are all true,
  ports may be numbers, and a list may be a JSON array, a JSON-encoded array in a query string, or a single bare
  string.
* **Which profile.** `profile_id` / `user_id` (the id) win over `profile_no` / `serial_number` (the serial number).
* **Secrets.** A proxy password is returned as `********`.

## Routes

| Method and path | Purpose |
|---|---|
| `GET /status` | liveness; no auth |
| `GET /api/v1/browser/start` | open a profile (v1) |
| `POST /api/v2/browser-profile/start` | open a profile (v2) |
| `GET /api/v1/browser/stop`, `POST /api/v2/browser-profile/stop` | close a profile |
| `GET /api/v1/browser/active`, `POST /api/v2/browser-profile/active` | is it open? |
| `GET /api/v1/browser/local-active`, `GET /api/v2/browser-profile/local-active` | every open profile |
| `POST /api/v1/user/create`, `POST /api/v2/browser-profile/create` | create a profile |
| `POST /api/v1/user/update`, `POST /api/v2/browser-profile/update` | change a profile |
| `POST /api/v1/user/delete`, `POST /api/v2/browser-profile/delete` | delete profiles and their data |
| `GET /api/v1/user/list`, `POST /api/v2/browser-profile/list` | list profiles |
| `GET /api/v1/group/list`, `POST /api/v1/group/create` | groups |

### Open a profile

```sh
# v1: query string; open_tabs=1 means "do NOT reopen the last tabs"
curl -s -H "Authorization: Bearer $KEY" \
  "http://127.0.0.1:50325/api/v1/browser/start?user_id=k5mf18c&headless=1"

# v2: JSON body; last_opened_tabs defaults to true
curl -s -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"profile_id":"k5mf18c","headless":"1","last_opened_tabs":"1"}' \
  http://127.0.0.1:50325/api/v2/browser-profile/start
```

Parameters: `profile_id` | `user_id` | `profile_no` | `serial_number`; `headless`; `launch_args` (list of kernel flags that
replace the profile's own); `open_tabs` (v1) / `last_opened_tabs` (v2).

```json
{"code": 0, "msg": "success", "data": {
  "ws": {"puppeteer": "ws://127.0.0.1:41337/devtools/browser/6f1c…", "selenium": "127.0.0.1:41337"},
  "debug_port": "41337",
  "webdriver": ""
}}
```

`ws.puppeteer` is the browser's CDP endpoint (Puppeteer `connect({browserWSEndpoint})`, Playwright
`connectOverCDP`), `ws.selenium` is the debugger address for chromedriver's `debuggerAddress`. `webdriver` is the
path of a chromedriver next to the kernel, empty when there is none (Clearcote does not ship one; set
`CITRO_CHROMEDRIVER`). Opening a profile that is already open returns its existing endpoints, so the call is
idempotent. Launches are serialised, so two simultaneous calls never start two browsers.

```python
# Playwright (Python)
from playwright.sync_api import sync_playwright
import requests

h = {"Authorization": f"Bearer {KEY}"}
r = requests.post("http://127.0.0.1:50325/api/v2/browser-profile/start",
                  json={"profile_id": "k5mf18c"}, headers=h).json()["data"]
with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp(r["ws"]["puppeteer"])
    page = browser.contexts[0].pages[0]
    page.goto("https://iphey.com")
```

### Close, status

```sh
curl -s -H "Authorization: Bearer $KEY" -d '{"profile_id":"k5mf18c"}' -H 'Content-Type: application/json' \
  http://127.0.0.1:50325/api/v2/browser-profile/stop            # {"code":0,"data":{},"msg":"success"}
curl -s … -d '{"profile_no":"1"}' …/api/v2/browser-profile/active  # {"status":"Active", "ws":…} or {"status":"Inactive"}
curl -s … http://127.0.0.1:50325/api/v1/browser/local-active    # {"list":[{"user_id":…,"profile_id":…,"ws":…,"debug_port":…}]}
```

Closing sends SIGTERM so Chromium flushes cookies and the session, and kills the process group only if the browser
is still there after ten seconds.

### Create and update

```sh
curl -s -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{
        "name": "shop-fr-2",
        "group_id": "1",
        "remark": "second shop",
        "platform": "https://www.example.com",
        "tabs": ["https://www.example.com"],
        "country": "fr",
        "user_proxy_config": {"proxy_soft":"other","proxy_type":"socks5","proxy_host":"203.0.113.9",
                              "proxy_port":"1080","proxy_user":"alice","proxy_password":"…"},
        "fingerprint_config": {"webrtc": "proxy", "hardware_concurrency": 8}
      }' http://127.0.0.1:50325/api/v2/browser-profile/create
# v2 -> {"profile_id":"kj4o4r2","profile_no":"2"}     v1 (/api/v1/user/create) -> {"id":"kj4o4r2","serial_number":"2"}
```

Accepted fields: `name` (≤ 100 characters), `group_id`, `remark`, `platform` / `domain_name`, `username`, `password`,
`fakey`, `ip`, `country`, `category_id` / `sys_app_cate_id`, `cookie`, `tabs` / `open_urls`, `launch_args`,
`user_proxy_config`, `fingerprint_config` (an object, merged key by key: see
[docs.md](../docs.md#6-the-persona-what-a-profile-claims-to-be)). Update takes the same fields plus the profile key;
it validates on a copy first, so a rejected request leaves the stored profile untouched.

**Not supported on purpose:** AdsPower's saved proxies (`proxyid`). The request is rejected with a clear message
rather than silently ignored, so a profile never ends up connecting directly when you asked for a proxy. Pass
`user_proxy_config` instead. A `group_id` that does not exist is rejected too (`0` and empty mean *ungrouped*).

### Delete

```sh
curl -s … -d '{"profile_id":["k5mf18c","kj4o4r2"]}' http://127.0.0.1:50325/api/v2/browser-profile/delete
```

Accepts `profile_id`, `user_ids`, `user_id` or `profile_no` (a list). The browser data directory is removed with the
profile. An unknown id answers `code: -1`.

### List

```sh
curl -s -H "Authorization: Bearer $KEY" "http://127.0.0.1:50325/api/v1/user/list?page=1&page_size=50&group_id=1"
curl -s -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"profile_id":["k5mf18c"],"limit":5,"user_sort":{"last_open_time":"desc"}}' \
  http://127.0.0.1:50325/api/v2/browser-profile/list
```

Filters: `profile_id` / `user_id`, `profile_no` / `serial_number`, `group_id`. Sort: `user_sort` as an object or a JSON
string, keys `serial_number` (default, newest first), `last_open_time`, `last_close_time`, `created_time`, direction
`asc` or `desc`. Paging: `page` (from 1) and `page_size` (v1) or `limit` (v2); **the default page size is 1 and the
maximum 100**, as in AdsPower. A listing first reconciles the run records, so `last_close_time` is current even for
a browser that was closed by hand.

Each row:

```json
{"serial_number":"1","profile_no":"1","user_id":"k5mf18c","profile_id":"k5mf18c","name":"shop-fr-1",
 "group_id":"1","group_name":"Shops FR","domain_name":"","platform":"","username":"","password":"",
 "remark":"first shop","sys_app_cate_id":"","created_time":"1794826565","last_open_time":"0",
 "last_close_time":"0","ip":"","ip_country":"fr",
 "user_proxy_config":{"proxy_soft":"other","proxy_type":"socks5","proxy_host":"203.0.113.9",
                      "proxy_port":"1080","proxy_user":"alice","proxy_password":"********"}}
```

`password` is the *account* password you stored for the target platform (AdsPower compatible); if you share the API
key, do not store account passwords in profiles.

### Groups

```sh
curl -s … -d '{"group_name":"Shops FR","remark":"french shops"}' -H 'Content-Type: application/json' \
  http://127.0.0.1:50325/api/v1/group/create        # {"group_id":"1","group_name":"Shops FR"}
curl -s … "http://127.0.0.1:50325/api/v1/group/list?page=1&page_size=10&group_name=shops"
```

## Differences from AdsPower

| AdsPower | Citroenition |
|---|---|
| saved proxies (`proxyid`) | not supported, rejected explicitly; pass `user_proxy_config` |
| SunBrowser / Orbita kernels | the open-source Clearcote kernel |
| cloud sync, team features of the vendor | `citro-hub`, your own server ([HUB.md](HUB.md)) |
| endpoints beyond the table above (extensions, categories, RPA, …) | not implemented: they answer 404 |

## Errors you will meet

| `msg` | Meaning |
|---|---|
| `profile_id (or profile_no) is required` | no profile key in the request |
| `profile not found: <key>` | no such id or serial number |
| `saved proxies (`proxyid`) are not supported: pass `user_proxy_config` instead` | see above |
| `group … does not exist` | unknown `group_id` |
| `invalid JSON body: …` / `the request body must be a JSON object` | malformed request |
| `the kernel exited before opening its DevTools port (status …): <log tail>` | the browser started and died; the tail of `kernel.log` says why |
| `timed out waiting for the kernel's DevTools endpoint` | the browser did not come up in time |
