# Citroenition manual

Citroenition runs **many isolated browser profiles**, each with its own proxy, its own cookies and its own coherent
"machine" (operating system, hardware, screen, GPU, language, time zone), from one command line, one desktop app or
one HTTP API. It is written in Rust. The only part that is not Rust is the browser engine, which has to be a
Chromium build: that engine is [Clearcote](https://github.com/clearcotelabs/clearcote-browser), an open-source
Chromium whose fingerprint controls are compiled into the engine.

This is the complete manual. The other documents go deeper on one subject each:

| Read | When you want to |
|---|---|
| [docs/API.md](docs/API.md) | drive profiles from Selenium / Puppeteer / Playwright over the AdsPower-compatible Local API |
| [docs/KERNEL.md](docs/KERNEL.md) | understand the browser engine, the anti-detect model, the measured WebGL recipe and the verification results |
| [docs/HUB.md](docs/HUB.md) | run the team server: users, teams, cross-device profile locks, hosted cookies, shared logs, CSV |
| [docs/GUI.md](docs/GUI.md) | use the desktop app |
| [docs/REVERSE_ENGINEERING.md](docs/REVERSE_ENGINEERING.md) | see how the AdsPower formats were measured, and the evidence behind them |

## Contents

1. [How the pieces fit](#1-how-the-pieces-fit)
2. [Install](#2-install)
3. [A five minute tour](#3-a-five-minute-tour)
4. [Concepts](#4-concepts)
5. [Command reference](#5-command-reference)
6. [The persona: what a profile claims to be](#6-the-persona-what-a-profile-claims-to-be)
7. [Proxies](#7-proxies)
8. [Language, time zone and location follow the proxy](#8-language-time-zone-and-location-follow-the-proxy)
9. [Where everything is stored](#9-where-everything-is-stored)
10. [Teams, locks and hosted cookies](#10-teams-locks-and-hosted-cookies)
11. [Logs](#11-logs)
12. [Verification: how we know it works](#12-verification-how-we-know-it-works)
13. [Security notes](#13-security-notes)
14. [Troubleshooting](#14-troubleshooting)
15. [Developing Citroenition](#15-developing-citroenition)

---

## 1. How the pieces fit

```
                     ┌────────────────────────────── your machine ──────────────────────────────┐
                     │                                                                          │
   citro-gui ───────▶│                    citro-core  (library)                                  │
   (desktop app)     │   profile store · persona model · launcher · process tracking            │
   citro (CLI) ─────▶│                          │                                               │
   citro serve ─────▶│                          ▼                                               │
   (Local API :50325)│        Clearcote kernel  (one process tree per open profile)             │
                     │        --user-data-dir=<home>/profiles/<id>/data                          │
                     └───────────────────────────────┬──────────────────────────────────────────┘
                                                      │ HTTPS, authenticated, optional
                                                      ▼
                                         citro-hub  (your own server)
                           users · teams · groups · cross-device locks · cookie hosting · logs · CSV
```

* **`citro-core`** is the library under everything. It owns the profile store, turns a profile into a kernel
  command line (the *persona*), starts and stops the browser and tracks it.
* **`citro`** is the command line, and `citro serve` is the **AdsPower-compatible Local API**.
* **`citro-gui`** is the desktop app (egui, pure Rust). It talks to a backend trait with two implementations:
  the local one (this machine's store) and the hub one (a team server).
* **`citro-install`** downloads, signature-checks and unpacks the Clearcote kernel. `citro kernel install` calls it.
* **`citro-hub`** is the team server. The *server* decides who may open a profile: a profile that is open on one
  device cannot be opened on another one, not even through the API.

A profile is plain data. It does not belong to a program: the CLI, the GUI and the API all read and write the same
store, under one lock.

## 2. Install

Requirements: **Linux x86-64** (glibc 2.35 or newer) or a **Mac with Apple silicon on macOS 12 or newer**. A headful
browser needs a display (on Linux a desktop session or Xvfb; a Mac always has one); headless works without one. On
Linux the desktop app needs an X11 session (XWayland on a Wayland desktop) and OpenGL; on a Mac it is an ordinary
native window. Clearcote also publishes a Windows build, which Citroenition does not drive yet, and its macOS build is
for Apple silicon only: an Intel Mac can run Citroenition only with a kernel you provide yourself (`--kernel`).

### 2.1 Get the programs

**Download a release.** The [releases page](https://github.com/afftroen/Citroenition/releases/latest) has one archive
per platform, each with `citro`, `citro-gui`, `citro-hub` and `citro-install`, and one `SHA256SUMS` file that covers
both. On Linux (glibc 2.35 or newer: Ubuntu 22.04, Debian 12 or later):

```sh
V=0.1.6
curl -LO https://github.com/afftroen/Citroenition/releases/download/v$V/citroenition-v$V-x86_64-linux.tar.gz
curl -LO https://github.com/afftroen/Citroenition/releases/download/v$V/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS      # citroenition-v0.1.6-x86_64-linux.tar.gz: OK
tar xzf citroenition-v$V-x86_64-linux.tar.gz
export PATH="$PWD/citroenition-v$V-x86_64-linux:$PATH"
```

On a Mac (Apple silicon, macOS 12 or newer):

```sh
V=0.1.6
curl -LO https://github.com/afftroen/Citroenition/releases/download/v$V/citroenition-v$V-aarch64-macos.tar.gz
curl -LO https://github.com/afftroen/Citroenition/releases/download/v$V/SHA256SUMS
grep aarch64-macos SHA256SUMS | shasum -a 256 -c      # citroenition-v0.1.6-aarch64-macos.tar.gz: OK
tar xzf citroenition-v$V-aarch64-macos.tar.gz
export PATH="$PWD/citroenition-v$V-aarch64-macos:$PATH"
open citroenition-v$V-aarch64-macos/Citroenition.app      # the desktop app
```

The macOS archive carries the desktop app as `Citroenition.app` (and `citro-gui`, a link to the program inside it).
The programs are signed *ad hoc*, which is what lets Apple silicon start them, but not with an Apple developer
identity and not notarised. A file that came through `curl` starts without a word. One downloaded in a browser
carries macOS's quarantine flag and is refused with "Apple cannot check it for malicious software": remove the flag
once with `xattr -dr com.apple.quarantine citroenition-v$V-aarch64-macos`, or use *System Settings → Privacy &
Security → Open Anyway*.

**Or build from source.** This needs a recent stable Rust toolchain and a C compiler (`gcc`, or `build-essential`, on
Linux; on a Mac `xcode-select --install`):

```sh
git clone https://github.com/afftroen/Citroenition
cd Citroenition
cargo build --release
export PATH="$PWD/target/release:$PATH"      # citro, citro-install, citro-hub, citro-gui
```

TLS in every crate is `rustls` with the `ring` provider: there is no OpenSSL anywhere. `ring` compiles a little C and
assembly, which is why the build wants a C compiler. On Linux the programs themselves need only glibc (2.35 or newer)
and `libgcc_s`, which every glibc Linux has; on a Mac only the system libraries.

### 2.2 Install the browser kernel

```sh
citro kernel install
```

This asks GitHub for the latest Clearcote release for your platform, downloads the archive (about 117 MiB for the
Linux build, 218 MiB for macOS), verifies it, and unpacks it to `<home>/kernels/clearcote-<version>/`:

1. the release's `SHA256SUMS.txt` must carry a **valid OpenPGP signature** from the signing key pinned inside
   `citro-install` (the key's fingerprint is printed by `citro-install verify-checksums`);
2. the archive's SHA-256 must match the signed list;
3. the archive is unpacked by Rust code that refuses absolute paths, `..` components, links that point outside the
   tree, device nodes, setuid bits and archive bombs (entry-count and size caps), creates files with `O_EXCL`, and
   writes into a staging directory that is renamed to `clearcote-<version>` only when everything succeeded.

Nothing runs before it has been verified. A second `citro kernel install` is a no-op (`--force` reinstalls).
`citro kernel list` shows what is installed. Several versions can sit side by side; the newest is used unless you
pass `--kernel PATH` or set `CITRO_KERNEL`.

**On a Mac** the kernel is `Chromium.app`, and the program to point `--kernel` at is
`Chromium.app/Contents/MacOS/Chromium`. Clearcote has published a macOS build for one release only so far
(`v0.1.0-pre.24`, Chromium 150; the newer releases have none), so on a Mac the installer takes the newest release
that has an archive for your platform instead of failing on the newest release. The unpacker keeps what the app
bundle is made of (symbolic links, executable bits) and never sets the quarantine flag.

`citro-install` is also a standalone program (`citro-install latest`, `install`, `list`, `verify-checksums`), with
`--base-url` for a mirror: the signature is still checked against the pinned key.

### 2.3 The home directory

All state lives under one directory: `~/.local/share/citroenition` (or `$XDG_DATA_HOME/citroenition`). Override it
with `--home DIR` or `CITRO_HOME`. See [section 9](#9-where-everything-is-stored) for the layout.

## 3. A five minute tour

This is real output from a fresh home.

```console
$ citro group new "Shops FR"
1
$ citro new "shop-fr-1" --proxy socks5://alice:s3cret@203.0.113.9:1080 --country fr --os linux \
        --group "Shops FR" --remark "first shop" --tab https://www.example.com
k5mf18c
created profile 1 (shop-fr-1); open it with: citro open k5mf18c
$ citro new "direct-test" --proxy none
kj4o4r2
created profile 2 (direct-test); open it with: citro open kj4o4r2
$ citro ls
NO  ID       NAME         GROUP      PROXY                      STATUS
1   k5mf18c  shop-fr-1    Shops FR   socks5://203.0.113.9:1080  -
2   kj4o4r2  direct-test  Ungrouped  direct                     -
$ citro group ls
ID  NAME      PROFILES  REMARK
1   Shops FR  1
$ citro kernel list
151.0.7922.108      <home>/kernels/clearcote-151.0.7922.108/chrome
```

(The first line of `new`'s output, the id, goes to stdout so a script can capture it; the sentence goes to stderr.)

`citro open 1` starts the browser for profile 1 (the argument is the id or the serial number). `citro open 1
--dry-run` prints exactly what would be started instead, with the secrets hidden:

```console
$ citro open 1 --dry-run
{
  "args": [
    "--user-data-dir=<home>/profiles/k5mf18c/data",
    "--remote-debugging-port=0",
    "--no-first-run", "--no-default-browser-check", "--password-store=basic", "--disable-sync",
    "--disable-breakpad", "--disable-dev-shm-usage", "--disable-background-networking", …,
    "--disable-features=Translate,BrowsingTopics,BrowsingTopicsDocumentAPI,Fledge,…,WebBluetooth",
    "--fingerprint=k5mf18c",                  <- the seed: one seed is one machine
    "--fingerprint-platform=linux",           <- the OS the profile claims
    "--fingerprint-brand=chrome",
    "--accept-lang=fr-FR,fr,en-US,en",        <- Chrome's own list for a French locale
    "--lang=fr",
    "--timezone=Europe/Paris",                <- from --country fr
    "--webrtc-ip-handling-policy=disable_non_proxied_udp",
    "--proxy-server=socks5://203.0.113.9:1080",
    "--disable-quic",                         <- QUIC is UDP; a TCP proxy cannot carry it
    "--socks5-credentials=<hidden>",
    "--no-sandbox",                           <- as AdsPower's Linux build; --sandbox keeps it
    "--test-type",                            <- silences the --no-sandbox infobar
    "--ignore-gpu-blocklist",                 <- keeps WebGL on, see docs/KERNEL.md
    "https://www.example.com"
  ],
  "env": { "FONTCONFIG_FILE": ["<home>/cache/fonts-151.0.7922.108.conf"], "LANGUAGE": ["fr"] },
  "kernel": "<home>/kernels/clearcote-151.0.7922.108/chrome",
  "version": "151.0.7922.108"
}
```

Close it with `citro close 1`. The window position, cookies, local storage and extensions persist in the profile's
data directory; `citro open 1` again restores the last session (use `--fresh` to start on the start pages).

Drive it from code (Local API):

```sh
citro serve --api-key "$(head -c24 /dev/urandom | base64)" &       # 127.0.0.1:50325
curl -s -H "Authorization: Bearer $KEY" "http://127.0.0.1:50325/api/v1/browser/start?user_id=1"
# {"code":0,"data":{"ws":{"puppeteer":"ws://127.0.0.1:41337/devtools/browser/…","selenium":"127.0.0.1:41337"}, …}}
```

## 4. Concepts

**Profile.** One identity: a name, a group, a remark, start pages, a proxy, a persona, and a browser data directory
(cookies, history, local storage, extensions). Every profile has an *id* (a short random string, stable forever) and
a *serial number* (1, 2, 3, …, handy to type). Both work wherever a profile is expected.

**Group.** A flat label for organising profiles (`Shops FR`, `Warm-up`, …). A profile is in one group or in
`Ungrouped`.

**Persona.** The "machine" the profile claims to be: operating system, browser brand and version, hardware
concurrency, device memory, screen, GPU, canvas / audio / client-rect noise, fonts, language, time zone,
geolocation, WebRTC address. The persona is **derived, not stored**: one *seed* (default: the profile id) plus a
handful of explicit choices produce the whole machine inside the engine. The same profile is the same machine every
time, and two profiles are two different machines. See [section 6](#6-the-persona-what-a-profile-claims-to-be).

**Kernel.** The browser engine: Clearcote Chromium. Citroenition installs and verifies it but does not bundle it.

**Running state.** An open profile is recorded in `<home>/run/<id>.json` (pid, DevTools port, kernel). The record is
trusted only while the process is alive *and* its command line still carries that profile's `--user-data-dir`, so a
recycled pid is never mistaken for a browser. A browser that was closed by hand, crashed or was killed is noticed
and its **last closed** time is recorded.

**Last opened / last closed.** Unix timestamps stored on the profile and shown by the CLI, the API
(`last_open_time`, `last_close_time`) and the GUI.

## 5. Command reference

Global options (accepted by every command): `--home DIR` (env `CITRO_HOME`) and `--kernel PATH` (env
`CITRO_KERNEL`).

### Profiles

| Command | What it does |
|---|---|
| `citro new NAME [options]` | create a profile, print its id |
| `citro list [--group G] [--json]` | list profiles (alias `ls`) |
| `citro show PROFILE` | the full profile as JSON, secrets hidden |
| `citro export [--out FILE] [--group G] [--no-bom]` | all profiles as a CSV every spreadsheet opens ([below](#csv-export)) |
| `citro edit PROFILE [options]` | change a profile; applies the next time it is opened |
| `citro delete PROFILE…` | delete profiles **and their browser data** (alias `rm`) |
| `citro group new NAME` / `citro group ls` | create / list groups |

`new` and `edit` share these options (`edit` also has `--name NAME` to rename):

| Option | Meaning |
|---|---|
| `--group G` | group id or name |
| `--remark TEXT` | free text shown in lists |
| `--tab URL` | start page; repeat for several (replaces the list) |
| `--platform URL` | the site this profile is for, opened when there is no `--tab` |
| `--proxy URL` | `[scheme://][user:pass@]host:port` with scheme `http`, `https`, `socks4` or `socks5`; or `none` |
| `--country CC` | ISO country of the proxy exit; picks the default language and time zone |
| `--os windows\|macos\|linux` | the operating system the profile claims (default: this machine's own) |
| `--ua UA` | user agent; normally leave it alone, the engine derives a consistent one |
| `--lang TAG` | language, most preferred first; repeat: `--lang fr-FR --lang fr` |
| `--timezone TZ` | IANA time zone, e.g. `Europe/Paris` |
| `--screen WxH` | screen / window size, e.g. `1920x1080` |
| `--fp KEY=VALUE` | any `fingerprint_config` key, JSON value when it parses: `--fp webrtc=proxy --fp hardware_concurrency=8` |
| `--arg FLAG` | extra kernel flag, repeat for more: `--arg=--disable-gpu` |

### CSV export

```sh
citro export --out profiles.csv                 # every profile
citro export --group "Shops FR" --no-bom | less # one group, to standard output, for scripts
```

One header row, then one row per profile, 24 columns in this order:

| Columns | Content |
|---|---|
| `no`, `id`, `name`, `group`, `remark` | serial number, id, name, group *name* (empty = ungrouped), remark |
| `persona_os`, `country`, `language`, `timezone` | the claimed OS, ISO country (upper case), and the language and time zone the profile pins, or that its country implies |
| `proxy_type`, `proxy_host`, `proxy_port`, `proxy_user` | the proxy without its password; empty for a direct connection |
| `tabs` | start pages joined with ` \| `; any `user:password@` inside a URL is masked as `user:***@` |
| `account_username` | the login name you stored for the target platform |
| `created_at`, `last_open_at`, `last_close_at` | ISO 8601 UTC, e.g. `2026-11-16T11:13:38Z`; empty = never |
| `status` | `open` or `closed` |
| `created_by`, `last_open_by`, `last_close_by`, `locked_by`, `locked_device` | filled by the team hub's export, empty in a local export |

The file is RFC 4180 (comma separated, CRLF line ends, quotes doubled) in UTF-8 **with a byte order mark**, so
Excel, Numbers, LibreOffice and Google Sheets all show accents correctly; `--no-bom` leaves it out for scripts. A
text cell that would start a formula in a spreadsheet (`=`, `+`, `-`, `@`, tab) gets a leading `'`, so a hostile
remark cannot run anything when the file is opened.

**Never in the file:** account passwords, 2FA keys, cookies, proxy passwords. The columns are written from a
fixed list, so a bug elsewhere cannot add one. `proxy_user` *is* in it: if your proxy provider puts a token in the
user name, treat the file as confidential.

### Running

| Command | What it does |
|---|---|
| `citro open PROFILE` | open the profile in its own browser; if it is already open, print its endpoints |
| `citro close PROFILE…` | close cleanly (SIGTERM so cookies and session are flushed; SIGKILL for the process group if it lingers) |
| `citro ps [--json]` | running browsers |

`open` options: `--headless` (headless=new; WebGL through SwiftShader), `--fresh` (start pages instead of the last
session), `--sandbox` (keep Chromium's sandbox; needs unprivileged user namespaces), `--arg FLAG` (flags for this
launch only, replacing the profile's own), `--timeout SECS` (wait for the DevTools endpoint, default 30), `--wait`
(block until the browser exits), `--dry-run`, `--json`.

### Kernel and API

| Command | What it does |
|---|---|
| `citro kernel [status]` | the kernel `open` would use |
| `citro kernel list` | installed kernels, newest first |
| `citro kernel install [--version V] [--force]` | download, verify and install Clearcote |
| `citro serve [--bind ADDR] [--api-key KEY]` | the Local API; default `127.0.0.1:50325`; the key can also come from `CITRO_API_KEY`; it is mandatory off loopback |

## 6. The persona: what a profile claims to be

You rarely need to set anything: `citro new NAME --proxy … --country fr` gives a profile whose language, time zone,
Accept-Language header and WebRTC behaviour agree with its proxy and whose hardware agrees with itself.

The profile's `fingerprint_config` object (the AdsPower field names, so existing tools keep working) and its OS
choice are mapped to the switches the engine reads:

| `fingerprint_config` / profile field | Effect |
|---|---|
| `persona_os` (`--os`) | `--fingerprint-platform` = `windows` / `macos` / `linux` |
| `seed` | `--fingerprint`; default is the profile id, so the identity is stable |
| `brand`, or an `Edg/` / `OPR/` user agent | `--fingerprint-brand` |
| `language`, `language_switch`, `--country` | `--accept-lang`, `--lang`: Chrome's own defaults for that locale |
| `automatic_timezone`, `timezone`, `--country` | `--timezone` |
| `latitude`, `longitude` | `--fingerprint-location` |
| `screen_resolution` (`1366_768`) | `--fingerprint-screen-width/-height`, `--window-size` |
| `hardware_concurrency`, `device_memory` | `--fingerprint-hardware-concurrency`, `--fingerprint-device-memory` |
| `webgl` = `2` with `webgl_config` | `--fingerprint-gpu-vendor`, `--fingerprint-gpu-renderer` |
| `webgl` = `0` | `--disable-gpu-fingerprint`: the machine's real GPU |
| `canvas` = `0` / `webgl_image` = `0` | `--disable-canvas-noise` / `--disable-fingerprint-noise` |
| `webrtc` | `--webrtc-ip-handling-policy`, `--webrtc-ip` |

**Ignored on purpose:** `ua` (beyond choosing the OS and the brand), `audio`, `client_rects`, `do_not_track` and
`fonts`. The engine derives them from the seed, and a hand-written value could only contradict the persona.

**Which OS to claim.** On a Linux host, the Linux and macOS personas pass iphey.com at 100 %; the Windows persona
is currently flagged on iphey's *Browser* tile (a Windows claim has to survive font, GL and shader-dialect checks
that a Linux host gives away). The default (`--os` omitted) is the host's own OS, which is also the most
consistent choice. On a Mac that default is the macOS persona. The measurements above and in section 12 were taken on
a Linux host: the Mac build has not been through iphey, BrowserScan and CreepJS yet.

## 7. Proxies

`--proxy` takes `[scheme://][user:pass@]host:port`; credentials may be percent-encoded. Supported schemes:
`http`, `https`, `socks4`, `socks5` (default `http`). `none` means a direct connection.

What Citroenition does when a proxy is set:

* `--proxy-server=<scheme>://host:port`, no credentials in the URL;
* `--disable-quic`: QUIC is UDP, a TCP proxy cannot carry it, and a connection that falls back is a timing tell;
* credentials go to the engine as `--socks5-credentials` (SOCKS5) or `--proxy-auth` (HTTP, HTTPS);
* **when the engine has neither switch** (Clearcote's macOS build 150), a SOCKS5 or HTTP proxy with a login goes
  through a **local relay**: Citroenition starts a small SOCKS5 server on `127.0.0.1:<random port>` (the launching
  program itself, run again with `__citro-proxy-relay`; it gets the proxy over stdin, never on its command line), the
  browser gets `--proxy-server=socks5://127.0.0.1:<port>`, and the relay logs in to the real proxy for every
  connection (SOCKS5 username/password, or HTTP `CONNECT` with `Proxy-Authorization`). Host names reach the proxy
  unresolved, so DNS does not leak. The relay exits when its browser does; its line is in the profile's
  `kernel.log`. An `https://` proxy with a login is not relayed;
* WebRTC is restricted to the proxied path (`disable_non_proxied_udp`) unless the profile says otherwise.

**Credentials and `ps`.** A command line is readable by every local user (`ps`; on Linux `/proc/<pid>/cmdline`). When the kernel
supports Clearcote's `--persona-from-env` switch, Citroenition moves the seed, every `--fingerprint-*` switch and
the proxy credentials into the `CLEARCOTE_PERSONA_ARGS` environment variable (private to the process) and keeps
them off the command line. The Clearcote 151 build does **not** have that switch yet (it is detected from the
binary, no version guessing), so with it the credentials are on the command line. On a machine other people log
in to, run Citroenition under its own user. Everything Citroenition itself prints, logs or stores in a log file has
the credentials replaced by `<hidden>`.

## 8. Language, time zone and location follow the proxy

Genuine Chrome derives everything from one value, the OS locale. It resolves it to a UI locale it ships
(`de-AT` → `de`, `en-CA` → `en-GB`, `es-MX` → `es-419`); `navigator.languages` and the `Accept-Language` header
are then that UI locale's built-in list, and `Intl` reports the UI locale itself. A persona that follows the same
rule looks like a fresh Chrome profile on that OS, whereas a hand-written language list is one of the most common
tells. Citroenition follows the rule: `--country de` gives `de-DE,de,en-US,en`, UI locale `de`, time zone
`Europe/Berlin`. On Linux the process environment gets `LANGUAGE=<ui locale>` too, because Chrome takes its UI
locale from there.

## 9. Where everything is stored

```
<home>/                       default ~/.local/share/citroenition   (--home, CITRO_HOME)
├── store.json                profiles and groups; mode 0600; written atomically (tmp file + rename)
├── store.lock                advisory lock: CLI, GUI and API can all write at once without losing edits
├── local_api                 address of a running `citro serve`
├── kernels/
│   └── clearcote-151.0.7922.108/
│       ├── chrome            the engine
│       ├── fonts/            the bundled metric-compatible font set and its fontconfig template
│       └── INSTALLED.json    version, archive SHA-256, source URL, install time
├── profiles/<id>/
│   ├── data/                 Chromium's --user-data-dir: cookies, local storage, extensions, session
│   └── kernel.log            the browser's stdout/stderr (append; mode 0600)
├── run/<id>.json             a live browser: pid, debug port, ws endpoint, kernel
└── cache/                    generated fontconfig files
```

Back up `store.json` and `profiles/`. Deleting a profile removes its directory, run record and log.

## 10. Teams, locks and hosted cookies

Several people can use Citroenition against **your own server**. `citro-hub` provides:

* **users, roles and teams** (owner / admin / member / viewer; a user can be in several teams), groups per team;
* **profile locks the server enforces**: a profile open on one device cannot be opened on another one, not even by
  the same user through the API. A lock is a short lease that the opener's device keeps renewing; it expires if the
  device dies, and an admin can force it free (audited);
* **hosting of cookies and profile data** on that server: the data blob is only readable and writable by the device
  that holds the lock, so two machines can never race on the same cookie jar;
* **logs** for the browser and for Citroenition, **remarks**, **last opened / last closed** per profile;
* **CSV export** of a team's profiles in the same 24-column format as `citro export`
  ([above](#csv-export)), with the `*_by`, `locked_by` and `locked_device` columns filled in.

Everything about running and using it is in [docs/HUB.md](docs/HUB.md).

## 11. Logs

* **Browser log**: `<home>/profiles/<id>/kernel.log`, whatever the kernel prints. When an open fails, the error
  shown by the CLI, the API and the GUI includes the tail of this file.
* **Citroenition events** (GUI): opened / closed / failed / exported / locked, with the profile and a timestamp,
  filterable and exportable.
* **Hub logs**: `GET /v1/logs`, both kinds, per team and per profile, with secrets scrubbed (see the hub docs).
* The Local API logs one line per failed request through `tracing` (`RUST_LOG=citro=debug` for more).

## 12. Verification: how we know it works

The claim "this is an anti-detect browser" is checked against third-party detectors with the real Rust launcher
(`citro open`), the real Clearcote 151 kernel and a real SOCKS5 proxy that requires a login, on a server with no
GPU. The harness is `tests/e2e/iphey.py`; it prints everything with the proxy login redacted and exits 0 only if the
browser's exit IP is the proxy's.

| Checker | Result |
|---|---|
| [iphey.com](https://iphey.com) | **Trustworthy**, MX score **100** in both runs. Browser *Chrome*, Location *Germany*, IP address the proxy's, Hardware and Software *Everything is fine* |
| [BrowserScan](https://www.browserscan.net) | Bot Detection: *No*, Proxy: *No*. Authenticity **95 %** in one run and **85 %** in the other (see below) |
| [CreepJS](https://abrahamjuliot.github.io/creepjs/) | **0 % headless**, **0 % stealth**, 38 % "like headless"; WebRTC blocked |
| stock Chromium 157 (control, same host and proxy) | iphey MX score 90: the engine scores higher than an unmodified browser |

BrowserScan's deductions: `WebGL exception −5 %` in both runs (the host has no GPU and presents an Intel GPU
identity over a software rasteriser), and `Different time zones −10 %` in the second run only. That second one is
the checker's own data, not the browser's: in that run BrowserScan's IP database located the exit IP in *Prague,
Czech Republic* while it had located the same IP in Germany in the first run, and iphey and our own lookup both say
Frankfurt. The browser reported `Europe/Berlin` every time.

Page-side values in the same run: `navigator.webdriver` is `false`, the user agent is
`Chrome/151.0.0.0 … Linux x86_64`, the language `de-DE` with `["de-DE","de","en-US","en"]`, the time zone
`Europe/Berlin`, and the browser's exit IP is the proxy's.

The screenshots are from the 95 % run:

![iphey](docs/screenshots/verify-iphey.png)
![BrowserScan](docs/screenshots/verify-browserscan.png)
![CreepJS](docs/screenshots/verify-creepjs.png)

The exit IP is masked in these images (`tools/re-scripts/mask_shots.py`). Re-run it yourself:

```sh
export CITRO_TEST_PROXY='socks5://host:port:user:pass'      # your own proxy; never commit it
python3 -I tests/e2e/iphey.py out/ --os linux                # installs the kernel with citro kernel install
```

Honest limits: the Windows persona is still flagged on iphey's Browser tile on a Linux host; the checkers' IP
geolocation databases disagree with each other and with themselves over time, which moves scores by ±10 % without any
change in the browser; the detectors change, so re-run the harness after any launcher change. The second run used
the kernel that `citro kernel install` had just downloaded, with no path given, so the installer, the launcher and
the engine are verified together. More in [docs/KERNEL.md](docs/KERNEL.md#verification).

## 13. Security notes

* **Secrets at rest.** `store.json`, run records and logs are created with mode `0600`. A proxy password is stored
  in `store.json` in clear text, as every profile manager must, so protect the home directory (disk encryption,
  own user, backups encrypted). `citro show` and the API print passwords as `********`.
* **Local API.** Bound to `127.0.0.1` by default. Off loopback `citro serve` refuses to start without `--api-key`.
  With a key every route except `/status` needs `Authorization: Bearer <key>`. Anyone who can reach the API can
  open any profile and read its cookies through DevTools: treat the key like a root password and never expose the
  port without TLS.
* **Command lines.** With the current Clearcote build (151) the proxy login and the seed are on the browser's command
  line, which every local user can read with `ps` (on Linux in `/proc/<pid>/cmdline`). On a shared Linux machine
  mount `/proc` with `hidepid=2` or run Citroenition under a dedicated user; macOS has no `hidepid`, so on a Mac use a
  dedicated user account or keep the machine to yourself. A kernel with `--persona-from-env` moves them into the
  process environment automatically.
* **DevTools.** The browser's debugging port listens on `127.0.0.1` on a random port, for the Local API's
  clients. Other users on the same machine can reach it; see the note on shared machines in [section 7](#7-proxies).
* **Kernel supply chain.** The kernel is verified against a pinned OpenPGP key and a signed checksum list before a
  byte of it is unpacked. Pass `--kernel` to use a build you compiled yourself.
* **The hub** stores only SHA-256 hashes of tokens, shows a token once, rate-limits authentication and scrubs
  secrets from logs; see [docs/HUB.md](docs/HUB.md#security-model).
* **Hygiene.** The launcher removes every `CITRO_*` variable from the browser's environment, so your hub token
  and API key are never visible to pages' child processes.

## 14. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `no kernel found` | run `citro kernel install`, or point at one with `--kernel PATH` / `CITRO_KERNEL` |
| the browser exits at once, error shows `kernel.log` | read the tail printed with the error; the commonest causes are no display (Linux: `DISPLAY` unset, use Xvfb or `--headless`) and a missing sandbox permission (the default is `--no-sandbox` as in AdsPower; `--sandbox` needs unprivileged user namespaces) |
| `timed out waiting for the kernel's DevTools endpoint` | a slow machine or a first start that is building caches: raise `--timeout 90` |
| a page has **no WebGL** / BrowserScan shows `WebGL exception` | on a host without a GPU WebGL needs `--ignore-gpu-blocklist` (always passed). Do **not** add `--use-angle=gl-egl`: it disables WebGL on display-only hosts |
| iphey's *Browser* tile is red | you are on a Linux host with `--os windows`; use `--os linux` (or `macos`) |
| proxy login keeps being asked / pages do not load | check the scheme (`socks5` vs `http`), the credentials, and that the proxy allows your IP; `citro open --dry-run` shows the flags with the login hidden |
| a hub operation answers `409` with code `locked` (the CLI: *is open on another device: anna on anna-laptop*) | the profile is open on another device; the answer names the holder. Wait for them to close it, or a team owner or admin runs `citro hub unlock` ([HUB.md](docs/HUB.md#troubleshooting)) |
| macOS: "Citroenition.app" or a program "cannot be opened because Apple cannot check it for malicious software" | the quarantine flag that a browser puts on downloads: `xattr -dr com.apple.quarantine <the folder>`, once. The kernel that `citro kernel install` unpacks never carries it |
| `address already in use` from `citro serve` | another Local API (or AdsPower itself!) owns `:50325`; use `--bind 127.0.0.1:50326` |
| two edits seem to overwrite each other | they cannot: every write takes `store.lock`; reload the list |

## 15. Developing Citroenition

```sh
cargo build                 # everything
cargo test                  # unit and integration tests
cargo clippy --all-targets  # must stay clean
cargo fmt --all             # rustfmt.toml: max_width 120
```

* Crates: `citro-core`, `citro` (CLI + API), `citro-install`, `citro-hub`, `citro-gui`.
* The installer's tests run against signed fixtures and a local HTTP server; nothing in the test suite needs the
  network except `tests/e2e`.
* `tests/e2e/iphey.py` is the real-world proof of [section 12](#12-verification-how-we-know-it-works);
  `tests/e2e/run.py` drives the Local API against a real kernel with a credential-demanding proxy and reads
  back what the page and the origin saw.
* `tools/re-scripts/` holds the measurement scripts used to learn the AdsPower formats
  ([docs/REVERSE_ENGINEERING.md](docs/REVERSE_ENGINEERING.md)).
* Rules the code follows: no secrets in logs or error messages, atomic writes for every file another process may
  read, no `unwrap` on user input, one place per concern (the launcher builds the command line, nothing else does).
