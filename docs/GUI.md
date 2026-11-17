# The desktop app

`citro-gui` is the desktop app: one window to create, group, open and export profiles, install the browser kernel,
read the logs and work with a team. It is written in Rust (egui), ships as one binary and starts without a runtime or a
browser engine of its own.

It does nothing the command line cannot do. Both are thin layers over the same crates, so a profile opened from the
app is locked on the team hub exactly like one opened with `citro open`, the kernel installer is the one behind
`citro kernel install`, and the Local API (`citro serve`) and the app can run side by side on one data directory.

![The Profiles page](screenshots/profiles.png)

All screenshots in this guide are taken from the app's own demo mode (`--demo`, fake data) with the app's own
`--screenshot` switch ([how](#taking-your-own-screenshots)), so they show the real interface and nobody's real
profiles.

## Contents

1. [Starting it](#starting-it)
2. [The window](#the-window)
3. [Profiles](#profiles)
4. [New and edit](#new-and-edit)
5. [Groups](#groups)
6. [Logs](#logs)
7. [Kernel](#kernel)
8. [Teams](#teams)
9. [Settings](#settings)
10. [Export to CSV](#export-to-csv)
11. [Quitting with team browsers open](#quitting-with-team-browsers-open)

## Starting it

```sh
citro-gui                              # the default data directory, ~/.local/share/citroenition
citro-gui --home /srv/citro-data       # another one (env CITRO_HOME)
citro-gui --kernel /opt/clearcote/chrome   # use this kernel binary for this run
citro-gui --demo                       # fake in-memory data; nothing is read from or written to disk
```

| Option | Meaning |
|---|---|
| `--home DIR` | the data directory (default: `$CITRO_HOME`, else `~/.local/share/citroenition`) |
| `--kernel PATH` | use this browser kernel binary for this run |
| `--demo` | fake in-memory data: nothing is read from or written to disk. Cannot be combined with `--home` (exit code 2) |
| `--screenshot OUT.png` | draw the window, write a PNG and exit. It needs a display (on a server: `xvfb-run`) |
| `--page PAGE` | the page for `--screenshot`: `profiles`, `groups`, `logs`, `logs-browser`, `kernel`, `teams`, `settings`, `edit`, `new`, `export`, `error` (default `profiles`) |
| `--size WxH` | window size (default 1280x800, minimum 960x600) |
| `--theme dark\|light` | colour theme for this run (default: the saved setting) |
| `-V`, `--version` | print the version and exit; this needs no display |
| `-h`, `--help` | the list of options |

Both `--flag value` and `--flag=value` work. An unknown option prints the usage and exits with code 2.

On Linux it needs an X11 session (on a Wayland desktop it runs through XWayland) and OpenGL; Mesa's software
renderer is enough. The libraries it loads at start-up (`libxkbcommon`, `libGL`, the X11 client libraries) are on
every desktop Linux. On a server without them, use `citro`: everything the app does is in the command line too.

On a Mac it is an ordinary native window (macOS 12 or newer, Apple silicon): open `Citroenition.app` from the
release archive, or run `citro-gui` from a terminal. See [docs.md §2.1](../docs.md#21-get-the-programs) for the
first-start warning that a browser download triggers.

The app shares its data directory with `citro` and `citro serve`, so the profiles, the groups, the logs and the team
login are the same in all three. Its own preferences live in `<home>/gui-settings.json` (never written in demo mode).

## The window

The window has a sidebar on the left, a strip with a few buttons on top, and the current page on a rounded card.

**The sidebar** (220 px) starts with the logo and the blue **New profile** button (Ctrl/Cmd + N). The pages come in
three groups: **Profiles** (a badge shows the number of profiles); under *Browser*, **Kernel** (a red `!` badge when
no kernel is installed, `new` when a newer one exists) and **Logs**; under *Workspace*, **Teams** and **Settings**.
Groups has no entry of its own: it is the second tab of the Profiles card.

At the foot is a small card that says where the data comes from, with a tag: *Demo data* (*Demo*), *This device*
(*Local*) or the team's name (*Online* or *Offline*). Under it come the number of profiles, how many are open and the
kernel version (a red *not installed* when there is none). When the kernel is missing or a newer one exists, the
card also has an **Install kernel** / **Update kernel** button that opens the Kernel page. The last line is the app's
version.

**The top strip** holds, from the right: the account chip (who you are and where: *Demo mode*, *Local*, or the hub
user and the team; hovering shows the data directory, a click opens Teams), the **theme** toggle (sun or moon: dark
and light), **Reload** (F5) and, while something runs in the background, a spinner.

![The same page in the light theme](screenshots/profiles-light.png)

Every page starts with its tab strip: *Profiles | Groups*, *Citroenition | Browser* on Logs, or just the page's
name. The page's own buttons sit at the right end of the strip (*Export CSV* on Profiles, *Check again* on Kernel,
*Pull now* and *Disconnect* on Teams, *Revert* and *Save settings* on Settings). Under the Profiles table a line
reads *Total: 14*, followed by *· 6 match the filters* and *· 2 selected* when they apply.

The window title is *Citroenition*. It opens at 1280x800 and does not shrink below 960x600.

**Toasts.** Results appear as small messages in the bottom right corner (at most five at a time; a click dismisses
one). Errors stay for 9 seconds, warnings for 6, the rest for 4. Things that happen without anybody waiting for them,
such as a browser that was closed by its own window and uploaded its data afterwards, show up here too, and the same
lines are written to the Citroenition log.

**Refreshing.** The profile list and the groups reload every 2 seconds, the kernel and the team every 15. **F5**
reloads everything.

**Keyboard.** Shortcuts are off while a dialog is open.

| Key | What it does |
|---|---|
| Ctrl/Cmd + N | new profile (goes to Profiles and opens the editor) |
| Ctrl/Cmd + F | focus the search box (goes to Profiles) |
| F5 | refresh everything, the logs included |
| Ctrl/Cmd + A | select all visible profiles |
| Esc | clear the selection; in the search box: clear it and leave |
| Up / Down | move the cursor; with Shift, extend the selection |
| Enter | open the selected profiles |
| Delete | delete the selected profiles (asks first) |

Ctrl+A, the arrows, Enter, Delete and Esc act on the Profiles page, and only when no text field has focus.

## Profiles

The page you work in. Its card starts with the *Profiles | Groups* tabs and, at the right, **Export CSV** (the
[export dialog](#export-to-csv)). Under that are the filters: a **group filter** (*All groups*, *Ungrouped* or one
group), the **search box** (name, remark and proxy; the ✕ or Esc clears it) and a **status filter** (*All*, *Open*,
*Closed*, *Locked*). **New profile** is the blue button at the top of the sidebar; it opens the
[profile editor](#new-and-edit).

Below the filters is the action bar: a blue **Open** button, then square icon buttons (hover one for its name) and, at
the right, how many rows are selected and a reload button.

| Button | What it does |
|---|---|
| **▶ Open** | open the selected profiles (Enter); enabled when a selected one is closed |
| **Close** | close the selected browsers |
| **Edit** | edit the selected profile; enabled when exactly one is selected |
| **Move to group** | move the selection to a group, or create one on the spot |
| **Set remark** | write the same remark on every selected profile |
| **Share with team** | only with a team hub connected: put the selected profiles on the hub |
| **Delete** | delete the selected profiles (Del) |
| **Reload** (right) | reload everything (F5) |

The table has these columns; each one sorts by a click on its header (a second click reverses; the two time columns
sort newest first), and every column can be resized:

| Column | Shows |
|---|---|
| # | the serial number: this device's shorthand for `citro open 3` and the Local API's `serial_number` |
| Name | the avatar, the name (with a blue **Team** mark in front when the profile is shared through the hub) and the profile id under it |
| Group | the group, or a dim *Ungrouped* |
| Proxy | a scheme badge and `host:port`, or *Direct*; under it the country and the exit address the persona follows. The tooltip shows the URL with the password masked (`scheme://user:***@addr`) |
| Remark | the remark's first line; hover for all of it |
| OS | *Auto*, *Windows*, *macOS* or *Linux* |
| Opened, Closed | relative times (*3 min ago*), the exact time on hover, *never* before the first use |
| Status | see below |

The last column of a row holds that profile's **Open** or **Close** button and a ⋮ menu.

**Status** is one of:

| Shown | Meaning |
|---|---|
| green **Open** | the browser runs here (the tooltip names the start time and the process id) |
| **Closed** | nothing is running |
| **Opening… / Closing… / Working…** | a spinner while it happens. *Closing* of a team profile stays until the data is on the hub |
| purple **Locked · ben** | somebody else has this team profile open; the tooltip says on which device since when. The Open button is disabled |
| red **Open failed** | the last start failed; the tooltip has the first line of the error |

**Selecting.** A click selects one row, Ctrl/Cmd + click toggles one, Shift + click selects a range, the checkbox in
the header selects everything that is visible. Bulk buttons only ever touch rows that are visible: a profile hidden
by a filter is never deleted by accident. A double-click opens a profile.

**Right-click** (or the ⋮ at the end of a row) opens the context menu: *Open*, *Close*, *Edit…* and *Duplicate* (one
profile), *Set remark…*, *Move to group…*, *Clear cache…*, *Share with team…* (with a hub connected), *Copy profile
id*, and *Delete…* in red. With several rows selected the labels carry the count.

**When a profile will not open** the app does not guess. A team profile that somebody else holds shows a warning
(*"Vinted FR - Louis is open on anna-laptop by anna since 3 min ago"*) and no browser starts. Any other failure opens a
window with the headline, the last lines of the kernel's own log, and the buttons **Close**, **Open browser log**
(jumps to [Logs → Browser](#logs) for that profile) and **Copy details**. Secrets in the message are masked.

**Delete** asks first. For a local profile it says the profile, its cookies, cache and saved logins are removed from
this device and cannot be recovered. For a profile that belongs to the team the question changes and the button says
**Delete for everyone**: the hub's copy goes too, for all members. See [the hub guide](HUB.md).

**Clear cache** removes the cached files of a profile and keeps the profile. Close the browser first.

An empty install shows *No profiles yet* with a **+ New profile** button and the command to do the same from a
terminal; a filter that matches nothing offers **Clear filters**.

## New and edit

**New profile** in the sidebar (or Ctrl/Cmd + N), the **Edit** button above the table, or *Edit…* in a row's menu open
the profile editor. It takes the place of the page (**‹ Back** or **Cancel** returns to the list), like AdsPower's
editor: one long form in five sections, with tabs at the top that jump to a section and follow the scrolling, and an
**Overview** card on the right. **Create profile** / **Save changes** sit in a footer that never scrolls away;
Ctrl/Cmd + Enter saves too (Enter in the name field as well). Esc leaves only a form you have not touched. When Save
finds a problem the page jumps to the first section that has one.

The Overview shows what sites will see: it is computed live from the form by the same code the launcher uses
(`citro_core::fingerprint::Resolved`), so it never shows a value the kernel would not apply. **New fingerprint** (top
right, or **New** next to the seed) picks a fresh seed, which changes every derived value at once.

![The New profile page](screenshots/new.png)

**General.**

* *Name*: required, at most 100 characters (for example *Vinted FR - Camille*).
* *Browser*: Chrome, Microsoft Edge, Opera or Vivaldi (`brand`): the name and version tokens the browser reports.
* *OS*: *Auto* (this computer's OS), *Windows*, *macOS* or *Linux*. The Linux and macOS personas pass iphey.com at
  100 % ([the evidence](../docs.md#12-verification-how-we-know-it-works)); the Windows persona is currently flagged on
  Linux hosts, which the hint says when it is chosen.
* *User-Agent*: empty = automatic. A custom one (`ua`) picks the OS and brand; the version is the installed kernel's.
* *Group*: a group, or *Ungrouped*; **+ New group** creates one without leaving the page.
* *Cookie*: JSON, Netscape or `name=value`. Kept with the profile (masked like a password) and synced through the hub.
* *Remark*: a note that appears in the list, up to 2000 characters.

**Proxy.**

* *Quick paste*: `socks5://user:pass@host:port`, `host:port` or `host:port:user:pass`; the fields below stay in sync
  with it. The password is always shown as `***` in this box.
* *Proxy type* (*No proxy (direct)*, `http`, `https`, `socks4`, `socks5`), *Host : Port*, *Proxy username* and *Proxy
  password*. A stored password shows *saved – type to replace*, and **Forget** removes it. The page never holds the real
  password: saving sends the mask back and the stored value stays.
* **Check proxy** connects through the proxy and reports *Works · exit IP 203.0.113.21 · France · 184 ms*, with a
  button **Use FR as country**, or the reason it failed.
* *Country*: *Auto (from the proxy IP)* or a country; the hint shows the language and time zone that follow
  ([why](../docs.md#8-language-time-zone-and-location-follow-the-proxy)).

**Platform.**

* *Platform* (`domain_name`): opened on start when there are no start URLs. *Username* / *Password*: the account on
  that platform, stored (the password masked) and shared with the team, never typed in for you.
* *Start URLs*: one per line; a line without a scheme gets `https://`; `about:` pages stay as they are.

**Fingerprint.** Every control writes an AdsPower `fingerprint_config` key the launcher reads (see the table at the
top of `crates/citro-core/src/fingerprint.rs`); keys the page does not know are kept.

* *WebRTC*: *Disabled* (leak-proof, the default), *Replace (proxy IP)* or *Real*.
* *Timezone*, *Language*: *Based on IP* or *Custom* (a time zone such as `Europe/Paris`; a list such as `fr-FR, en`).
* *Location*: *Not set* or a custom latitude and longitude.
* *Display language*: *Based on language* or a custom UI locale.
* *Screen resolution*: *Based on seed* or a custom size, with common presets.
* *Hardware noise*: **Canvas** and **WebGL image** switches. AudioContext, ClientRects, media devices and speech voices
  always come from the seed.
* *WebGL metadata*: *Based on seed*, *Real* (this computer's GPU) or *Custom* vendor and renderer, with presets.
* *CPU cores*, *Memory*: *Based on seed* or a fixed value.
* *Fingerprint seed*: empty = the profile id, so a profile keeps one identity however often and wherever it is
  opened. **New** generates one, **Copy** copies the seed in use.

**Advanced.**

* *Launch arguments*: extra Chromium switches, one per line, each starting with `--`.

![The Edit profile page](screenshots/edit.png)

For a profile that belongs to the team the change is written through to the hub when you save, and refused while
somebody has the profile open.

## Groups

Groups keep related profiles together, for example one per shop, market or account family. The page is the second tab
of the Profiles card.

![The Groups page](screenshots/groups.png)

* **Create**: type a name (and an optional remark) in the bar on top and press **+ Create group** or Enter. A name
  that exists already, in any capitalisation, is refused.
* The table shows each group with its number of profiles and its remark. The first row, *Ungrouped*, is built in and
  only has **View**.
* **View** jumps to Profiles filtered on that group. **Rename** edits the name and the remark in place. **Delete**
  asks first; the group's profiles move to *Ungrouped* and are kept.

Groups are per device. A profile that goes on the team hub carries its group's *name*, and a pull creates a missing
group by that name.

## Logs

Two views of the same page, as tabs.

![The Citroenition log](screenshots/logs.png)

* **Citroenition**: what the app and the hub did: profiles opened and closed, exports, proxy checks, kernel installs,
  team events (*lock lost*, *upload finished*, *quit: …*).
* **Browser**: the kernel's own log for one profile, live. Pick the profile in the list; the default is the selected
  one, else a running one, else the first.

![The browser log of one profile](screenshots/logs-browser.png)

The controls: a **level** menu (*Everything*, *Debug and up*, *Info and up*, *Warnings and errors*, *Errors only*;
the Citroenition tab starts at *Info and up*), a text filter, a profile filter, **Refresh**, **Clear view** (only
hides what is on the screen; nothing is deleted), **Copy** (the shown lines) and **Follow** (stay at the bottom).
Right-click a line to copy it. The page fetches at most the newest 2000 lines and says so when the limit is hit; it
refreshes by itself every 1.5 seconds while it is open.

Each line has the local time (`MM-DD HH:MM:SS`), a coloured level chip, the profile name and the message; the
tooltip has the full time. A copied line reads `<time> LEVEL [Profile name] message`.

The team's own log, which also holds what the other members' devices wrote, is on the hub: see
[What is logged](HUB.md#what-is-logged).

## Kernel

The browser engine every profile runs on is the open-source Clearcote build. This page shows which one is used and
installs or updates it.

![The Kernel page](screenshots/kernel.png)

* **Active kernel** has a badge: *Not found*, *Update available: 152.0…*, *Up to date*, or *Ready* (installed, the
  newest not known). It shows the version, the **location**, *Chosen because* (the newest installed kernel, a path
  pinned in Settings, `--kernel`, or `CITRO_KERNEL`) and the *Newest available* build. With a path pinned in Settings
  it warns that newer installs are not picked up, with a button to Settings.
* **Install / update kernel** downloads the newest build, verifies it and unpacks it into `<data dir>/kernels`. A
  progress bar shows the download (*12.4 MB of 118 MB · 10 %*), then *verifying the signature and unpacking…*. Browsers
  that are open keep their build until you close and reopen them. It is the code behind `citro kernel install`: the
  list of checksums must carry a valid OpenPGP signature of the pinned key, the archive must match its entry, and the
  unpacking refuses unsafe paths. [More](KERNEL.md#installing-and-replacing-the-kernel).
* **Installed kernels** lists every build under the data directory with the active one marked, and **Copy path**.
* **Check again** looks for a newer build; the answer is cached for ten minutes, a failed lookup is retried after
  twenty seconds, and offline simply shows *unknown*.

When no kernel is found, profiles cannot open and the Kernel entry in the sidebar carries a red `!`.

## Teams

Teams, the roles, the locking of profiles and the hosted cookies are explained in the [hub guide](HUB.md). This page
is the app's side of it.

**Not connected.** The page explains what a hub is, and asks for four things: the **hub address** (`https://…`;
`http://` is accepted on a local network), the **access token** your hub admin gave you (masked; Enter connects), the
**name of this device** (shown to teammates; the default is the one in Settings) and, only if your account is in
several teams, the **team**. **Connect** logs in and pulls the team's profiles. The form forgets the token the moment
the hub accepts it; the login itself, token included, is kept in `<home>/hub/login.json`, readable only by you,
and is shared with `citro`.

**Connected.**

![The Teams page](screenshots/teams.png)

* The top line has the green *Connected* dot, the hub address, who you are and on which device, and two buttons:
  **Pull now** (fetch the newest profiles, remarks and groups) and **Disconnect**.
* The team card shows the team's name, **your role** (Owner, Admin, Member or Viewer), the number of members and
  profiles, and the members with their roles.
* **Profile locks** lists every team profile that is open somewhere right now: the profile, who holds it, on which
  device and since when. Your own rows say *close it from Profiles*; the others have **Force unlock**, available to
  owners and admins only (the button is disabled with the reason for everybody else). The confirmation spells out the
  risk: the other browser may still be running, and two sessions can overwrite each other's cookies.
* **Pull now** also runs by itself every minute while the app is open. The lock list refreshes every 4 seconds. Neither
  waits for the network before the profile table is shown.

On the **Profiles** page the team shows as a blue **Team** badge, a **Locked · name** status, and a **Share with team**
button that uploads the selected profiles (settings, proxy, cookies and browser data) after a confirmation:
everybody in the team can then open them, one device at a time. The hub gives a shared profile a new id.

**Disconnect** is refused while team browsers are open or still closing (*Close the 2 open team browsers before
disconnecting: their data has to reach the hub first*). Otherwise the team's profiles stay on this computer but cannot
be opened until you connect again; nothing is deleted, here or on the hub.

A browser that outlived the app (a crash, a power cut) is taken back at the next start: the app finds it, takes the
lock again and supervises it, and says so in a notice.

## Settings

![The Settings page](screenshots/settings.png)

* **Updates**: this version and whether a newer one is out, with **Check for updates**. Citroenition asks GitHub
  for its newest release when it starts (and every six hours while it stays open). When there is one, the foot of the
  sidebar says **↑ Update to vX** and **Update…** opens the update dialog with the release notes: **Update now**
  downloads the archive for this computer from the repository's releases, checks its SHA-256 against the release's
  `SHA256SUMS`, unpacks it and swaps it into the folder the app runs from (all or nothing: on any error the previous
  version is put back). **Restart now** starts the new version; open browsers keep running and profiles, kernels and
  settings are not touched. This works for an unpacked release folder the app can write to; a development build or an
  app moved out of its folder is told to download the release instead. The demo never goes online.
* **Appearance**: *Dark* or *Light*, applied at once.
* **Opening browsers**:
  * *Headless: open browsers without a window*. For servers and automation.
  * *Keep Chromium's sandbox*. Safer, but the kernel then needs permission to create user namespaces; leave it off in
    containers or when you run as root.
  * *Kernel path (optional)*: pin one kernel binary. Empty means the newest installed one (or `$CITRO_KERNEL`). The
    line below the field says whether the path exists and is usable.
  * *Start CSV exports with a UTF-8 byte-order mark*: Excel needs it to show accented names correctly.
* **This device and its data**: the *device name* (what teammates see in lock messages and logs), the data directory,
  the settings file and the version, each with **Copy**.

**Save settings** is enabled when something changed; **Revert** takes the changes back. The page says *Unsaved changes*
or *All changes saved*.

## Export to CSV

**Export CSV** on the Profiles page writes the list to a file anyone can open.

![The Export dialog](screenshots/export.png)

* *What to export*: **Selected** (when something is selected), **All profiles**, or the **current group**. The default
  is the selection, else the group filter, else everything.
* *Write to*: a path (`~/citro-profiles-YYYYMMDD-HHMM.csv` to start with). There is no file picker; a missing folder is
  created, an existing file is replaced (the dialog warns first), and a folder is refused.
* The line under it states the format: *24 columns · UTF-8 · one row per profile · proxy passwords are never
  written*. The **byte-order mark** checkbox starts from the setting.

When it is done the dialog says *Wrote 3 profiles to /home/demo/citro-profiles-20261117-0000.csv* with **Copy path**.
The columns are those of `citro export`: [CSV format](../docs.md#csv-export). The team's hub
makes the same file for the whole team (`GET /v1/teams/{team}/profiles.csv`).

## Quitting with team browsers open

Closing the window while a *team* browser is open (or still closing) does not quit at once. The app cancels the close
and shows **Closing your team browsers**: their cookies and data are going up to the hub so that the next person gets
the newest state, and the window closes by itself when that is done (it waits up to three minutes per profile). The
quit is recorded in the Citroenition log.

Browsers of profiles that are not shared with a team are left alone: they are ordinary processes, and they keep
running when the app closes.

## Taking your own screenshots

The pictures in this guide come from the app itself, with no display and no real data:

```sh
citro-gui --demo --screenshot teams.png --page teams --size 1280x800
citro-gui --demo --screenshot export.png --page export --theme light
```

`--page` takes `profiles`, `groups`, `logs`, `logs-browser`, `kernel`, `teams`, `settings`, `edit`, `new`, `export`
and `error`. The app still draws a real window before it saves the picture, so it needs a display: on a server, run it
under `xvfb-run`. Without any display it stops with *neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set*.
