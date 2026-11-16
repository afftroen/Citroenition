# The browser kernel

Everything in Citroenition is Rust except the one thing that cannot be: a Chromium engine. The engine is
**[Clearcote](https://github.com/clearcotelabs/clearcote-browser)**, an open-source, de-Googled Chromium whose
fingerprint controls live inside the engine rather than in page scripts.

| | |
|---|---|
| Licence | BSD-3-Clause (checked against the repository's `LICENSE`: "Copyright (c) 2026, Clearcote Labs and the Clearcote contributors") |
| Version used | `151.0.7922.108` (release `v0.1.0-pre.25`); the project tracks Chromium stable, previous releases were 150 and 149 |
| Platforms of the project | Linux x64 and Windows x64 in every release, macOS arm64 in some. Citroenition drives Linux x64 |
| Integrity | every archive has a `.sha256` and an OpenPGP signature; `SHA256SUMS.txt` is signed too |
| What Citroenition ships | no browser at all. `citro kernel install` downloads it ([docs.md §2.2](../docs.md#22-install-the-browser-kernel)) |

Citroenition replaced AdsPower's proprietary SunBrowser kernel with this one; what was learnt from AdsPower is in
[REVERSE_ENGINEERING.md](REVERSE_ENGINEERING.md). Stock Chromium cannot be used instead: it has no persona
switches, so every property a detector reads (platform, GPU, screen, hardware, language) would be the real host's.

## How a profile becomes a browser

`crates/citro-core/src/launch.rs` is the only place that builds the command line. The persona is **derived**: one seed
and a few explicit choices go in, the engine produces a whole consistent machine.

| Group | Switches | Source in the profile |
|---|---|---|
| identity | `--fingerprint=<seed>`, `--fingerprint-platform`, `--fingerprint-brand` | seed (default: the profile id), `persona_os` / `--os`, brand or user agent |
| locale | `--accept-lang`, `--lang`, `--timezone`, env `LANGUAGE` | `--country`, `--lang`, `--timezone`; Chrome's own locale rules ([docs.md §8](../docs.md#8-language-time-zone-and-location-follow-the-proxy)) |
| location | `--fingerprint-location` | `latitude` / `longitude` |
| hardware | `--fingerprint-hardware-concurrency`, `--fingerprint-device-memory`, `--fingerprint-screen-width/-height`, `--window-size` | `fingerprint_config`, `--screen` |
| GPU | `--fingerprint-gpu-vendor`, `--fingerprint-gpu-renderer`, or `--disable-gpu-fingerprint` for the real GPU | `webgl`, `webgl_config` |
| noise | noise is on by default; `--disable-canvas-noise` and `--disable-fingerprint-noise` turn it off | `canvas` = `0`, `webgl_image` = `0` |
| network | `--proxy-server`, `--socks5-credentials` / `--proxy-auth`, `--disable-quic`, `--webrtc-ip-handling-policy`, `--webrtc-ip` | `user_proxy_config`, `webrtc` |
| browser | `--user-data-dir`, `--remote-debugging-port=0`, `--no-first-run`, `--password-store=basic`, `--disable-sync`, `--disable-features=Translate,BrowsingTopics,…`, `--no-sandbox` + `--test-type` (unless `--sandbox`), `--restore-last-session` (unless `--fresh`), `--headless=new` | the launch options |

Not passed, on purpose: `--enable-automation` (it sets `navigator.webdriver`), `--user-agent` (the engine derives a user
agent that matches platform, brand and version; a hand-written one can only contradict them).

Run `citro open PROFILE --dry-run` to see the exact list for a profile, secrets hidden.

### WebGL: the recipe, and the measurements behind it

A VPS, a container or an Xvfb display has no GPU the engine trusts, and without help the engine **disables WebGL
altogether**, which is a loud signal by itself. The launcher therefore chooses the GL backend from the OS the
persona claims (`gpu_backend_args` in `kernel_env.rs`):

| Claim | Flags | Why |
|---|---|---|
| any | `--ignore-gpu-blocklist` | keeps WebGL available on a host with no allow-listed GPU |
| Windows | plus `--use-angle=swiftshader-webgl`, env `CLEARCOTE_SHADER_DIALECT=hlsl` | SwiftShader reports the 4096 vertex-uniform limit real Direct3D 11 machines report (Mesa's OpenGL clamps to 1024), and the HLSL dialect makes `getTranslatedShaderSource()` look Direct3D as well |
| Linux, macOS | nothing else: the engine's default backend | measured on the real kernel: the default backend and `--use-angle=gl` both give working WebGL |
| never | `--use-angle=gl-egl` | measured: **no WebGL at all** on a display-only host |

A backend you choose yourself with `--arg=--use-angle=…` is kept; only the blocklist override is added. Remaining
weakness: a persona that presents an Intel GPU while the rasteriser is software costs 5 % on BrowserScan
(`WebGL exception`). A host with a real GPU should not have it; that is untested.

### Fonts

On a Windows claim the page must not see a Linux font list. The kernel archive carries a bundle of
metric-compatible font clones (`fonts/`, 40 files: Arimo for Arial, Selawik for Segoe UI, …) and a fontconfig
template. The launcher renders the
template into `<home>/cache/fonts-<version>.conf` and points `FONTCONFIG_FILE` at it, so a read-only or shared kernel
directory works.

### The persona transport

A command line is public on a multi-user machine. The Clearcote SDK passes the seed, every `fingerprint-*`
switch and the proxy credentials through the environment variable `CLEARCOTE_PERSONA_ARGS` and a bare
`--persona-from-env`. The launcher does the same **when the kernel supports it**; support is detected by looking for
the switch's name in the binary. Measured: the `151.0.7922.108` build does not contain it (`persona-from-env`: 0
matches in the 283 MB binary), so with this kernel the credentials go on the command line, and
[docs.md §13](../docs.md#13-security-notes) says how to live with that. The code path is unit-tested with a fake
kernel that carries the switch.

## Installing and replacing the kernel

* `citro kernel install` — download, verify (pinned OpenPGP key + SHA-256) and unpack to
  `<home>/kernels/clearcote-<version>/`, `INSTALLED.json` records version, hash and source.
* `citro kernel list` — what is installed; the newest is used.
* `--kernel PATH` or `CITRO_KERNEL` — use any binary. Nothing else changes: the launcher asks the binary for its
  version and for the switches it knows.

**Replacing it.** A kernel is acceptable if it understands the switches in the table above (at least
`--fingerprint*`, `--timezone`, `--accept-lang`, `--socks5-credentials` or `--proxy-auth`). To judge a candidate,
run the real-world check against it:

```sh
CITRO_TEST_PROXY='socks5://host:port:user:pass' python3 -I tests/e2e/iphey.py out/ --os linux --kernel-dir /path/to/extracted/kernel
```

If iphey flags the persona or the build proves unstable, swap the kernel and keep the Rust code: it is the only
component that depends on it, and `kernel.rs` / `kernel_env.rs` are the only files that know its switches.

## Verification

Run against iphey.com, CreepJS and BrowserScan with the real `citro open`, the real kernel and a SOCKS5 proxy that
needs a login, on a server with no GPU: iphey **Trustworthy, MX score 100** (twice), BrowserScan **95 % / 85 %**
(the 10 % difference is BrowserScan's own IP database placing the exit IP in two different countries; the
only browser-side deduction is the 5 % WebGL one), CreepJS **0 % headless, 0 % stealth**, stock Chromium 157 as the
control scoring **90** on iphey. Full table, screenshots and the honest limits are in
[docs.md §12](../docs.md#12-verification-how-we-know-it-works).

Findings from the measurement work that are worth keeping:

* The locale chain is the biggest tell. Chrome derives UI locale and `navigator.languages` from the OS locale; a
  persona that sets them independently is flagged. Following Chrome's own rule fixed it.
* On a Linux host the macOS persona passes iphey; the Windows persona is flagged on the *Browser* tile. The locale
  fix did not change that, and the cause is unproven (font, GL and shader-dialect checks are the suspects).
* `--use-angle=gl-egl` removes WebGL on hosts without a GPU; the default backend does not.
* A checker's IP geolocation moves scores more than the browser does.
