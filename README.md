# Citroenition

An open-source **anti-detect browser profile manager**, written in Rust: many isolated browser profiles, each with
its own proxy, its own coherent "machine" (OS, hardware, screen, GPU, language, time zone) and its own cookies,
opened with one click, one command or one API call.

It speaks the **AdsPower Local API** (`http://127.0.0.1:50325`), so existing Selenium / Puppeteer / Playwright
automation written for AdsPower points at it unchanged. The browser engine is
[**Clearcote**](https://github.com/clearcotelabs/clearcote-browser), an open-source Chromium build; Citroenition
does not bundle a browser, it installs and verifies one for you.

| | |
|---|---|
| **`citro`** | the command line + the AdsPower-compatible Local API server (`citro serve`) |
| **`citro-gui`** | the desktop app (pure Rust, egui): profile table, groups, remarks, last-opened / last-closed, logs, CSV export |
| **`citro-hub`** | the self-hosted team server: users and teams, cross-device profile locks the server enforces, cookie / profile-data hosting, logs, CSV |
| **`citro-install`** | downloads, signature-checks and unpacks the Clearcote kernel |
| **`citro-core`** | the library under all of it: profile store, persona model, launcher |

Everything above is Rust. The only non-Rust part is the browser engine itself, which has to be a Chromium build.

## Quick start

```sh
cargo build --release                 # needs a recent stable Rust toolchain
export PATH="$PWD/target/release:$PATH"

citro kernel install                  # download + verify (OpenPGP + SHA-256) + unpack Clearcote
citro new "shop-fr-1" --proxy socks5://user:pass@host:1080 --country fr --os linux
citro open 1                          # profile id or serial number
citro ls
citro close 1
```

Full manual: **[docs.md](docs.md)**. More: [kernel & anti-detect model](docs/KERNEL.md), [Local API](docs/API.md),
[desktop app](docs/GUI.md), [team hub](docs/HUB.md), [how AdsPower was reverse engineered](docs/REVERSE_ENGINEERING.md).

## Status

Verified against the real kernel through a real SOCKS5 proxy that needs a login: see
[docs/KERNEL.md](docs/KERNEL.md#verification) for what iphey.com, CreepJS and BrowserScan report.

## License

MIT, see [LICENSE](LICENSE). Clearcote is a separate project under its own licence and is downloaded, not bundled.
