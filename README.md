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

![Citroenition: the Profiles page](docs/screenshots/profiles.png)

## Quick start

Download the archive of the [latest release](https://github.com/afftroen/Citroenition/releases/latest) for your
machine (Linux x86-64, or macOS 12+ on Apple silicon), or build the programs yourself:

```sh
tar xzf citroenition-v0.1.6-x86_64-linux.tar.gz       # on a Mac: citroenition-v0.1.6-aarch64-macos.tar.gz
export PATH="$PWD/citroenition-v0.1.6-x86_64-linux:$PATH"

# or, from a clone: needs a recent stable Rust toolchain and a C compiler
cargo build --release
export PATH="$PWD/target/release:$PATH"

citro kernel install                  # download + verify (OpenPGP + SHA-256) + unpack Clearcote
citro new "shop-fr-1" --proxy socks5://user:pass@host:1080 --country fr --os linux   # --os macos on a Mac
citro open 1                          # profile id or serial number
citro ls
citro close 1
```

Full manual: **[docs.md](docs.md)**, what changed: [CHANGELOG.md](CHANGELOG.md). More: [kernel & anti-detect model](docs/KERNEL.md), [Local API](docs/API.md),
[desktop app](docs/GUI.md), [team hub](docs/HUB.md), [how AdsPower was reverse engineered](docs/REVERSE_ENGINEERING.md).

## Status

Verified against the real kernel through a real SOCKS5 proxy that needs a login, on Linux: see
[docs/KERNEL.md](docs/KERNEL.md#verification) for what iphey.com, CreepJS and BrowserScan report. The macOS build runs
the whole test suite and the real kernel on a Mac in the release workflow before every release; the anti-detect
measurements have not been repeated on a Mac.

## License

MIT, see [LICENSE](LICENSE). Clearcote is a separate project under its own licence and is downloaded, not bundled.
The desktop app embeds the [Inter](https://github.com/rsms/inter) font, which is under the SIL Open Font License 1.1:
the text is [crates/citro-gui/assets/fonts/Inter-LICENSE.txt](crates/citro-gui/assets/fonts/Inter-LICENSE.txt) and
the release archive carries it as `LICENSE-Inter.txt`.
