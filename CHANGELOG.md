# Changelog

## Unreleased

* **`deploy/install-hub.sh` and `deploy/expose-hub.sh`** put the team hub on an Ubuntu server: a `citro-hub` system user
  with no login, its data in `/var/lib/citro-hub` (`0700`), the checksum-verified release program owned by root, a
  sandboxed systemd service on `127.0.0.1:8470`, and then HTTPS through the server's nginx and certbot. Re-running the
  first one upgrades the program and keeps the data; a failure in the second puts nginx back as it was. See
  [docs/HUB.md](docs/HUB.md#install-on-an-ubuntu-server).

## 0.1.7 (2026-11-17)

This release also brings everything listed under 0.1.6 below: that build could not get a macOS runner from GitHub and
was never published.

* **Tags.** Profiles can carry several tags, each with a name of up to 50 characters and one of ten colors. A Tags
  column shows them as chips; clicking it ticks tags; the toolbar's tag button opens the Tags window (tick tags for the
  selection, create, recolor, delete). Imports bring tags along (a `tags` column, or `profile_tags` from a Local API).
* **Edit in the list.** Click a profile's group to move it (or the whole selection) to another group; double-click its
  name or remark to edit it in place (Enter saves, Esc cancels). Double-clicking a row no longer opens the browser.
* **Exports with cookies and start pages.** An antidetect browser's `.xlsx` export, which leaves large cookie lists out
  ("Cookie too large and exceeds Excel's grid limits"), is matched to the browser by each profile's id and the cookies
  are fetched through its Local API. Start pages (split only between addresses, so commas inside an OAuth link stay),
  the user agent and tags come from the file. Importing again over profiles imported before fills in what they lack
  instead of skipping them.
* **Local API**: `POST /api/v1/user/regroup`, `POST /api/v2/browser-profile/delete-cache` (cookie, local_storage,
  indexeddb, extension_cache, history, image_file), `GET /api/v2/browser-profile/cookies`, and `profile_tags` in profile
  lists.
* The repository names no other product any more: the import command is now `citro import api`
  (`$ANTIDETECT_API_KEY`), and the importer's default address is `http://127.0.0.1:50325`.

## 0.1.6 (2026-11-17)

**Import from the antidetect browser, quitting that really quits, bookmarks, and a profile list like other antidetect browsers'.**

* **Import profiles** (Settings -> Import profiles, or `citro import`): straight from a running antidetect browser through its
  Local API (groups, proxies with their logins, platform accounts and every profile's cookies; the antidetect browser does not hand
  out its fingerprints, so imported profiles get fresh ones), or from a file: CSV (comma, semicolon or tab), Excel
  `.xlsx`, JSON or a list of proxies. Drop a file on the window to read it. A preview shows every profile and
  warning first; **Save sample CSV** writes an example with every column. Profiles imported before from the same
  The antidetect browser profile are skipped.
* **A forgiving reader**: columns are matched by many names, proxies in every common notation, OS and country in loose
  spellings; a value that is almost right becomes a warning on its row instead of rejecting the file.
* **Quitting quits**: the red button (or Cmd+Q) closes every open browser the way Chrome quits, so cookies and open
  tabs are saved and come back next time, and then the app exits.
* **Bookmarks** for profiles and groups (`Name | URL` per line), put on the bookmarks bar before a launch; bookmarks
  added in the browser itself are kept.
* **The profile list** is laid out like other antidetect browsers': No., ID, the name with the OS mark and on two lines, group, the
  IP with its country, a wide two-line remark, the creation date, last opened (or what is happening now), Open.
* Cookies marked as session cookies but carrying a real expiry (the antidetect browser's) keep their expiry.

## 0.1.5 (2026-11-17)

**Extensions, group settings, default preferences, a search engine, and fixes for cookies and start pages.**

* **Chrome extensions.** A new Extensions page installs extensions from a Chrome Web Store link (or id) and loads
  them into every profile or into chosen groups (`--load-extension`). They keep the store's id (the store's public key
  goes into the manifest), so their data survives a moved data folder. CLI: `citro extension add|ls|rm`.
* **Group settings.** A group can have its own start pages and extensions (Groups -> Settings), opened and loaded in
  every profile of the group.
* **Defaults for new profiles** (Settings -> New profiles), like other antidetect browsers' Preferences: the profile editor without
  the per-profile fields; every new profile starts from them.
* **A search engine.** Clearcote ships without Google and searches nowhere from the address bar. Profiles now get
  Google by default (or Bing, DuckDuckGo, Brave, Yahoo, Ecosia, Startpage), written into Chromium's protected
  preferences with the MAC it checks; it can still be changed in the browser.
* **Cookies from the Cookie field are imported.** They used to be stored but never reached the browser. JSON,
  Netscape and `name=value` go in through DevTools on the next launch, once per change of the field.
* **Edited start pages open.** A restored session used to ignore the start pages; new or changed ones now open next
  to the restored tabs.
* **No more "needs to download the font" dialogs** on macOS: font auto-activation is turned off for the kernel.
* **Headless with several start pages** works (Chromium refuses more than one URL headless; the rest open through
  DevTools).
* **Move the data folder** (Settings): everything moves, a pointer file makes the app and `citro` follow.
* **A roomier profile editor**: wider label column, more space between label and field and between rows.

## 0.1.4 (2026-11-17)

**Proxies with a login work on macOS.**

* **The fix.** Clearcote's macOS build (150) has no `--socks5-credentials` or `--proxy-auth`, so a SOCKS5 or HTTP proxy
  that needs a username and password failed on a Mac with `ERR_SOCKS_CONNECTION_FAILED`. For such an engine
  Citroenition now runs a local relay per browser: the browser talks to `socks5://127.0.0.1:<port>` without a login and
  the relay logs in to the real proxy (host names stay unresolved, so no DNS leak). It ends with the browser. Engines
  that take the login as a switch (Linux) are launched exactly as before. Checked on a Mac with the real kernel and a
  real SOCKS5 proxy with a login: the page sees the proxy's exit IP.
* **No more keychain prompt on macOS.** Browsers start with `--use-mock-keychain`, as other antidetect browsers' do, so Chromium no
  longer asks for the login keychain password ("Chromium Safe Storage"). `--password-store=basic` only works on Linux.

## 0.1.3 (2026-11-17)

This release also brings everything listed under 0.1.2 below (the new profile editor and the logo): the 0.1.2 build
could not get a macOS runner from GitHub and was never published.

**Update from inside the app.** Citroenition looks for a newer release on GitHub when it starts; the sidebar then shows
**↑ Update to vX**, and Settings has an **Updates** card with **Check for updates**. **Update now** downloads the archive
for this computer, checks its SHA-256 against the release's `SHA256SUMS`, unpacks it with the kernel installer's
path-safe extractor and swaps it into the install folder, putting the old version back on any error; **Restart now**
starts the new version. The download code is `citro_install::app_update`, the swap is `citro_gui::updater`. Releases
are not signed yet, so the trust is TLS to the repository's own release downloads plus the checksum list.

## 0.1.2 (2026-11-17)

**A profile editor like other antidetect browsers', and a logo.**

* **New and Edit profile** is now a full page instead of a small dialog: five sections (General, Proxy, Platform,
  Fingerprint, Advanced) with tabs that jump between them, roomier rows, a footer that stays put, and a live
  **Overview** of what sites will see, computed by the launcher's own persona code.
* **Many more options**, each one an antidetect browser `fingerprint_config` key the launcher already applies: browser brand,
  custom user agent, WebRTC, time zone, location, languages, display language, screen resolution, canvas and WebGL
  image noise, WebGL vendor and renderer, CPU cores, memory and the fingerprint seed (with **New fingerprint**). Also
  cookie, platform domain / username / password and launch arguments.
* **The lemon logo** in the sidebar, as the window icon and as the macOS app icon (`Citroenition.app` gets
  `AppIcon.icns`).
* The Proxy column of the profile table keeps a one-line value (*Direct*) on the row's centre line.

## 0.1.1 (2026-11-17)

**macOS on Apple silicon.** The programs and the kernel now run on a Mac (M1 or newer, macOS 12 or newer). The release
has a second archive, `citroenition-v0.1.1-aarch64-macos.tar.gz`, and one `SHA256SUMS` that covers both archives.

* **The kernel.** `citro kernel install` fetches Clearcote's macOS build (`Chromium.app`, Chromium 150). The archive is
  a zip: the unpacker applies the same safety rules as for the Linux tarball and keeps what an app bundle is made of
  (symbolic links, executable bits). Clearcote has published a macOS build for one release only so far
  (`v0.1.0-pre.24`; the newer releases have none), so the installer takes the newest release that has an archive for
  the platform instead of failing on the newest one. That release's signed checksum list is a test fixture.
* **Process tracking** reads a process's arguments with `sysctl(KERN_PROCARGS2)` where Linux has `/proc`, and the
  device name comes from `gethostname`, so closing, lock tracking and the team features work the same on a Mac.
* **The desktop app** is `Citroenition.app` in the archive (and `citro-gui`, a link to the program inside it), signed
  ad hoc so that Apple silicon starts it. It is not notarised: a browser download carries macOS's quarantine flag, see
  [docs.md §2.1](docs.md#21-get-the-programs) for the one command that clears it.
* **The release is checked on a Mac before it is published.** The release workflow builds on an Apple-silicon runner,
  runs the whole test suite there, and runs the packaged programs against the real kernel: install it, open a
  profile, ask the browser who it is over DevTools, check that `citro ps` sees the process, close it.
* The Linux archive is built as before. The release workflow is now three jobs (Linux, macOS, publish) and the
  packaging, the release notes and the kernel smoke test are scripts in `scripts/`.

**Not on a Mac yet**

* Intel Macs: Clearcote publishes no Intel build. Use `--kernel` with a build you provide.
* The macOS kernel is Chromium 150, one major version behind the Linux and Windows builds.
* The anti-detect measurements (iphey.com, BrowserScan, CreepJS) were taken on Linux and have not been repeated on a
  Mac.
* The programs are not notarised and carry no Apple developer identity.

**A new look for the desktop app**, modelled on a reference design: a calm dark (and light) workspace instead of the
dense tool window. Nothing about what the app does has changed.

* A 220 px sidebar with the logo, a blue **New profile** button, the pages in *Browser* and *Workspace* sections, and a
  card at the foot with the data source, the profile and open counts, the kernel version and an **Install / Update
  kernel** button.
* A top strip with the account chip, the theme toggle and reload, and every page on one rounded card with its own tab
  strip (*Profiles | Groups*, *Citroenition | Browser*). Groups is now a tab of Profiles, not a sidebar entry.
* The profile table has taller rows with an avatar, the profile id under the name, the country and exit address under
  the proxy, status dots, an **Open** / **Close** button per row and a ⋮ menu. The action bar is a blue **Open** button
  and square icon buttons with tooltips. The old Country / IP column is folded into Proxy.
* The Inter font is embedded in three weights (Regular, Medium, SemiBold), with thin dropdown chevrons. Its licence
  (SIL OFL 1.1) ships in the release archive as `LICENSE-Inter.txt`.
* The edit dialog sizes the *Forget* button from its text, so it no longer touches the scrollbar.
* [docs/GUI.md](docs/GUI.md) and all its screenshots describe the new layout.

## 0.1.0 (2026-11-17)

The first release. Everything is Rust except the browser engine, which is the open-source Chromium build
[Clearcote](https://github.com/clearcotelabs/clearcote-browser): Citroenition installs and verifies it, it does not
bundle it.

**What is in the archive**

* `citro`: the command line, and `citro serve`, a Local API that is compatible with other antidetect browsers', so automation written
  for the antidetect browser (Selenium, Puppeteer, Playwright) points at it unchanged.
* `citro-gui`: the desktop app: profile table, groups, remarks, last-opened and last-closed times, logs, CSV export,
  the kernel installer, and everything about teams.
* `citro-hub`: the self-hosted team server.
* `citro-install`: downloads, signature-checks and unpacks the Clearcote kernel (`citro kernel install` does the same).

**What it does**

* **Profiles** with their own proxy, cookies and a persona that agrees with itself and with its proxy: operating system,
  hardware, language, time zone and location.
* **Teams** on your own server: users, the roles owner, admin, member and viewer, groups, remarks, and the last-opened
  and last-closed time of every profile.
* **Locks the server enforces.** A profile that is open on one device cannot be opened on another, by anyone, through
  any front end: the app, the command line and the Local API all ask the hub first. A lock is a short lease that the
  opener keeps renewing; it expires when the device dies, and a team owner or admin can break it (audited).
* **Cookies and browser data hosted on that server**, uploaded when a profile is closed and downloaded when it is
  opened. Only the device that holds the lock can write the data, and only that device (and team admins) can read it.
  A lost lock is never merged: what the losing device did is kept aside.
* **Logs** for the browser and for Citroenition, per team, with secrets scrubbed on the device and again on the hub.
* **CSV export** in one fixed 24-column format with a byte-order mark, so spreadsheets open it correctly. Passwords are
  never written.
* **A verified kernel install.** The checksum list must carry a valid OpenPGP signature from the key pinned in
  `citro-install`, the archive must match it, and unpacking refuses unsafe paths.

**Verified** with the real Clearcote kernel 151.0.7922.108, through a real SOCKS5 proxy that needs a login, on a server
with no GPU, with the Linux persona ([docs.md §12](docs.md#12-verification-how-we-know-it-works)):

* **iphey.com:** *Trustworthy*, MX score 100 in both runs; Hardware and Software "Everything is fine". Stock Chromium 157
  on the same host and proxy scores 90.
* **BrowserScan:** Bot Detection *No*, Proxy *No*, authenticity 95 % in one run and 85 % in the other. The 10 % is
  BrowserScan's own IP database placing the exit IP in two different countries; the only deduction that comes from the
  browser is 5 % for WebGL, because the host has no GPU.
* **CreepJS:** 0 % headless, 0 % stealth, 38 % "like headless"; WebRTC blocked.
* The browser's exit IP is the proxy's, `navigator.webdriver` is `false`, and language and time zone follow the proxy.
* The test suites, including the ones that run a real hub and several devices against it, pass.

The screenshots are in [docs/KERNEL.md](docs/KERNEL.md#verification).

**Known limits**

* Linux x86-64 only: process tracking uses `/proc` and POSIX signals. Other platforms build from source but are not
  supported.
* The desktop app needs an X11 session (XWayland on a Wayland desktop) and OpenGL.
* iphey.com currently flags the *Windows* persona on a Linux host. Use `--os linux` or `--os macos`.
* The 151 kernel does not carry the switch that passes proxy credentials through the environment, so the proxy
  password is on the kernel's command line and visible to other local users through `ps`
  ([docs.md §13](docs.md#13-security-notes) says how to live with that).
* `citro-hub` speaks plain HTTP: run it behind a TLS reverse proxy ([how](docs/HUB.md#behind-a-tls-reverse-proxy)).
  It does not encrypt the hosted cookies: protect the server like a password manager.

Start with [docs.md](docs.md); the team server is explained in [docs/HUB.md](docs/HUB.md).
