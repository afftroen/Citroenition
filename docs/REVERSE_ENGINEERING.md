# How the antidetect browser formats were learnt

Citroenition's profile model, `fingerprint_config` field names and Local API are **antidetect-browser-compatible** so that
existing scripts, spreadsheets and habits keep working. This page records where that compatibility comes from, how it
was measured, and what is deliberately *not* carried over. Nothing here is needed to use Citroenition; it is the
evidence trail for the code in `crates/citro-core` and `crates/citro`.

Scope: formats and behaviour needed for **interoperability**. No the antidetect browser source file, binary or asset is part of
this repository, and the browser kernel is a different, open-source one ([KERNEL.md](KERNEL.md)).

## Method

1. **Read what the app writes.** A local the antidetect browser install keeps per-profile config blobs under its `source/cache/`
   directory, one file per profile named by a 32-hex-digit hash. `tools/re-scripts/decode_cfg.py` decodes them and
   prints each profile's keys with every secret-looking value (`pass`, `account`, `token`, `cookie`, `key`,
   `proxyuser`, `address`, …) replaced by its length.
2. **Read how the app builds them.** The desktop app is Electron; its main bundle (`main.min.js`, from the `asar`
   archive) contains the functions that turn a `fingerprint_config` into kernel parameters. They were located by
   searching for the key names (`tools/re-scripts/grep_ctx.py`: context around each needle, long secret-like
   strings masked) and read, not copied.
3. **Black-box the kernel.** For every claim from steps 1-2 the proprietary kernel (SunBrowser, Chromium 157) was
   launched headless with *one parameter at a time*, and the value a page can see was read back over the DevTools
   protocol (`fp_probe.mjs`, `fp_matrix2.mjs`, `px_matrix.mjs`). A parameter is only recorded as "honoured" when
   the page-visible value changed.
4. **Port, then pin with tests.** Each rule became Rust with a unit test, the test vector taken from a real blob
   (for example the shuffled-base64 prefix check below).

## 1. The config blobs are base64 with a shuffled alphabet

Every blob the app hands to the kernel is RFC 4648 base64 with the 64 symbols permuted, `=` padding kept:

```
standard : ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/
reference : hTy1bfRJz4nLPcBCO7WtmNIaGvVeul5Zo8kq32UxrYw_-0gsjp96SDFXQiEMKdHA
```

Symbol *i* of the standard alphabet is replaced by symbol *i* of the second line. Real files start with `{"C…` and
therefore begin with `5941`, which is the unit test `matches_prefix_seen_in_real_config_files`
(`crates/citro-core/src/codec.rs`). The codec is kept in `citro-core` so the antidetect browser configs can still be read.

## 2. A profile reaches the kernel through one inline argument and three files

| Channel | Carrier | Holds |
|---|---|---|
| inline | `--extended-parameters=<blob>` on the command line | every parameter, plus the *paths* of the three files below |
| static file | `StaticConfig` (blob) | the inline keys except a few, plus `UserAgentMetadata`, `CanvasMarkEx`, `WebGLMarkEx`, `command_line`, `ProxyChain` |
| dynamic file | `DynamicConfig` (blob) | `TimeZone`, `Geoposition`, `WebRTCAddress`, `DisableWebRTC` |
| WebGL file | `WebGLFP` (plain JSON) | the unmasked vendor / renderer and extension list |

Keys seen include `UserId`, `Platform`, `HardwareConcurrency`, `DeviceMemory`, `ScreenSize`, `Langs`, `AcceptLang`,
`TimeZone`, `CanvasMark`, `AudioFp`, `ClientRectFp`, `WebGLMark`, `StartTime`, `CustomIcon`, `CookiesFile`.

### Which channel the kernel actually honours (measured, kernel 157)

| Page-visible value | Honoured from |
|---|---|
| `navigator.language(s)`, `Accept-Language` | static `Langs` (and `AcceptLang` for the header); inline `Langs` is ignored |
| `Intl` time zone | dynamic `TimeZone`; inline `TimeZone` is ignored |
| `platform`, `hardwareConcurrency`, `deviceMemory`, `doNotTrack`, `screen.*`, canvas and audio noise | inline |
| `userAgentData.platform` | static `UserAgentMetadata`, and only together with `--user-agent` |
| HTTP proxy authentication | inline `ProxyUser` / `ProxyPassword` **or** static `ProxyChain` (both work); credentials inside the `--proxy-server` URL are ignored |

Emitted by the app but with no effect observable on a headless test kernel: `ClientRectFp`, `WebGLFP`, `Geoposition`,
the `WebRTC*` keys. Known gap: the `Sec-CH-UA-Platform` request header still reported the host OS.

## 3. Behaviours worth knowing

* **argv is rewritten.** The kernel replaces its own argument vector with a single space-joined string (`argc == 1`).
  Finding "the process of profile X" therefore cannot compare NUL-separated arguments; it must match
  `--user-data-dir=<dir>` on token boundaries so `…/data` never matches `…/data2`. Citroenition keeps this rule: a run
  record is trusted only while the pid's command line still carries the profile's data directory.
* **`--enable-automation` is not passed**; it would set `navigator.webdriver`.
* **Default switch set.** The app launches every kernel with about two dozen switches: `--no-sandbox` on Linux,
  `--protected-disable-safe-open`, `--no-first-run`, `--no-default-browser-check`, `--password-store=basic`,
  `--use-mock-keychain`, `--disable-sync`, `--disable-breakpad`, `--disable-background-networking`,
  `--force-color-profile=srgb`, `--disable-blink-features=AutomationControlled`,
  `--disable-features=Translate,BackForwardCache`, … The first Citroenition launcher reproduced the set minus
  `--enable-automation`. The current launcher keeps the harmless part and adds its own privacy switches
  (Topics, Fledge, shared storage and Web Bluetooth off), see
  [KERNEL.md](KERNEL.md#how-a-profile-becomes-a-browser).
* **JA3.** The app toggles TLS extension permutation by the user agent's Chrome version: permuted for versions above
  105, not permuted below.
* **Environment.** `LANGUAGE` (UI locale) and `ISOLATE_DEFAULT_LOCALE` are set per launch.
* **Small functions ported with tests:** `getChromeVersion` (the `N` of `Chrome/N.x.y.z`), `getAccept` (the
  `Accept-Language` list with `q` weights), `hashcode` (`t = 31 * t + charCode` with JavaScript number semantics,
  no 32-bit wrap) and `setAudioNoise` (sum of char codes modulo 2000, minus 1000, never zero). `setWebRTC`'s
  "forward" mode additionally fetches STUN/TURN servers from the vendor's cloud, which is not reproduced.

With the move to the Clearcote kernel the SunBrowser-specific parts above (channels, `ProxyChain`, argv rewriting,
the JA3 toggle) are history; the *field names* survive as the input of the persona model
([docs.md §6](../docs.md#6-the-persona-what-a-profile-claims-to-be)).

## 4. The Local API

The Local API's shapes (default address `127.0.0.1:50325`, v1 query-string and v2 JSON routes, the
`{"code", "msg", "data"}` envelope, `ws.puppeteer` / `ws.selenium` / `debug_port` / `webdriver` in the start reply,
the `local_api` address file) follow the antidetect browser's public Local API documentation and the behaviour of the app. They are
specified route by route in [API.md](API.md). Where the antidetect browser accepts a loose input (numbers as strings, a list as a
JSON string) so does Citroenition; where the antidetect browser has a feature Citroenition does not (saved proxies, RPA, extension
management) the request is refused loudly instead of being half-honoured.

## 5. Reproducing the measurements

The scripts need a SunBrowser kernel (the one an antidetect browser install downloads), so they are evidence, not part of the
build. Each takes the kernel path from `KERNEL=…`:

```sh
KERNEL=/path/to/SunBrowser node tools/re-scripts/fp_matrix2.mjs          # one launch per case, prints the keys that differ from the base
KERNEL=/path/to/SunBrowser node tools/re-scripts/px_matrix.mjs           # which channel authenticates to a Basic-auth HTTP proxy
python3 -I tools/re-scripts/decode_cfg.py                                # decode and summarise an install's config blobs (secrets masked)
python3 -I tools/re-scripts/grep_ctx.py main.min.js 200 400 5 StaticConfig   # context around a key in the app bundle
```

The scripts that talk to the *current* kernel and the verification sites are described in
[KERNEL.md](KERNEL.md#verification) and live in `tests/e2e/`.

## Redaction rule

Anything read from an install may contain account names, proxy logins and cookies. The decoding scripts mask them
at the source, committed outputs carry lengths instead of values, and the verification screenshots in
`docs/screenshots/` have the exit IP blacked out by `tools/re-scripts/mask_shots.py`.
