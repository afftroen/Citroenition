# The team hub

`citro-hub` is the server that lets a team share Citroenition profiles. It is one program with one data
directory. It speaks plain HTTP, so you run it behind your own TLS reverse proxy.

What it gives a team:

* **One opener at a time.** A profile that is open on one device cannot be opened on another device or by another
  user, not even through the Local API. The server decides, not the apps.
* **Cookies and browser data on your own hub.** A profile's data lives on the hub and follows the profile from
  device to device. Caches stay on the device.
* **Secrets stay out of lists.** Passwords, 2-FA keys, cookies and proxy passwords are never handed out in a list.
  Only the device that holds the lock (and team admins) can read them.
* **Teams** with the roles owner, admin, member and viewer, **groups**, **remarks**, and the **last-opened** and
  **last-closed** time of every profile.
* **Logs** for the browser and for Citroenition itself, with secrets scrubbed.
* **CSV export** in a format everyone can read.

```
 anna's laptop  ─┐                                  ┌─ state.json   users, teams, profiles, locks
 ben's desktop  ─┼─ HTTPS ─▶ reverse proxy ─▶ citro-hub ─┼─ blobs/       hosted profile data, newest versions
 citro / citro-gui / citro serve                   └─ logs/        one log per team
```

This guide is for the person who runs the hub ([Running a hub](#running-a-hub) to
[Backups and restore](#backups-and-restore)) and for the people who connect to it
([Connecting a device](#connecting-a-device)). [The HTTP contract](#the-http-contract) is for anyone who writes a
client or a script. The commands are in the [manual](../docs.md#10-teams-locks-and-hosted-cookies), the desktop side is in
[GUI.md](GUI.md).

## Contents

1. [Concepts](#concepts)
2. [Running a hub](#running-a-hub)
3. [Install on an Ubuntu server](#install-on-an-ubuntu-server)
4. [Users, teams and tokens](#users-teams-and-tokens)
5. [Behind a TLS reverse proxy](#behind-a-tls-reverse-proxy)
6. [Backups and restore](#backups-and-restore)
7. [Connecting a device](#connecting-a-device)
8. [The HTTP contract](#the-http-contract)
9. [What is logged](#what-is-logged)
10. [Security model](#security-model)
11. [Troubleshooting](#troubleshooting)

## Concepts

**User.** A name (letters, digits and `. _ - @`, at most 64 characters, unique when case is ignored) and a token.
A user has a *server role*: `admin` (the hub operator: creates teams, and counts as owner of every team) or
`member` (sees only the teams they were added to).

**Token.** The secret a user's devices send. A token is `citro_` followed by 64 hex digits (70 characters). It is
printed once, when it is created. The hub keeps only its SHA-256 hash, so a lost token cannot be recovered, only
replaced. A user has one token at a time: rotating it kills the old one at once; revoking the user is final.

**Team.** A wall around profiles, groups, members and logs. A user can be in several teams, with a different role
in each. A user who is not in a team gets `404` for everything about it and cannot tell that it exists.

**Roles.** Per team, from least to most:

| Role | May |
|---|---|
| `viewer` | read: profile lists (secrets masked), groups, members, logs, CSV export. Cannot open, edit or create |
| `member` | everything a viewer may, plus: open profiles (take the lock, pull and push the data), create and edit profiles, create and rename groups, leave the team |
| `admin` | everything a member may, plus: delete profiles and groups, break a lock, add and remove members (not owners), read a profile's secrets without holding its lock |
| `owner` | everything an admin may, plus: make and remove owners. A team always keeps at least one owner |

**Profile.** An ordinary Citroenition profile (the fields `citro show` prints) plus what the hub keeps about it:
its team, who created it, who opened and closed it last, its lock and the version of its data. The hub assigns the
`profile_id` (`k` and six characters) and a *serial number* that counts per team from 1 and is never reused. Those
two, the creation time and the last-opened / last-closed times belong to the hub: no request can set them. A
device numbers the profiles it pulls with its own serial numbers, so the number you type is a per-device
shorthand; the profile id is the same on every device, and the hub's serial number is the `no` column of the
hub's CSV export.

**Group.** A flat label inside a team. Names are unique per team when case is ignored. `0` is the built-in
*ungrouped*. Deleting a group moves its profiles to ungrouped; it never deletes profiles.

**Lock.** The statement "this profile is open, on this device, for this user". Taking the lock is the first step
of opening a profile, and there is at most one live lock per profile. The lock is exclusive: the same user on a
second device is refused as firmly as a stranger.

**Lease.** The proof of a lock. When the hub grants a lock it hands the opener a secret string (`ls_` and 48 hex
digits), sent back in the `X-Citro-Lease` header. The hub stores only its hash. Every action that needs the lock
(heartbeat, unlock, upload, edit, reading the secrets and the data) must present the lease and be the user it was
issued to. A lease lasts `--lease-ttl` seconds (default 90) and every heartbeat extends it; the apps send a
heartbeat every third of that time. If the heartbeats stop (a crash, a cut cable, a laptop that went to sleep) the
lease runs out and the profile is free again. A restarted app on the same device gets a fresh lease for the same
lock; the old lease is void at once. Leases survive a hub restart until they expire.

**Breaking a lock.** A team owner or admin can force a profile free. The old lease stops working everywhere at
once, and the hub writes an audit line naming who broke whose lock.

**Data version.** The hub keeps a profile's browser data as an archive (`.tar.gz` of the browser's data
directory) in numbered versions: 1, 2, 3, … The numbers only grow. The newest `--keep-versions` (default 5) are
kept. Only the holder of the lock can read or write the data. An upload says which version it is based on
(`base_version`), so an upload that is out of date is refused (`409 stale`) instead of overwriting newer data.
The hub checks the SHA-256 of every upload and installs it atomically, so nobody ever reads half a file.

**Caches stay home.** The archive holds what makes the browser the same browser on another device: cookies, local
storage, IndexedDB, saved logins, extensions, preferences, history, session. It leaves out caches (`Cache`,
`Code Cache`, `GPUCache`, shader caches, …), crash and metrics folders, the large component-updater downloads and
the browser's process lock files.

### The life of one profile

1. **Open.** The device asks the hub for the lock. If somebody else has it, the answer names them and the browser
   does not start. Otherwise the device gets a lease and starts heartbeating.
2. **Pull.** With the lease the device fetches the profile's real settings (with secrets) and, if the hub's data
   differs from what the device synced last, the newest data version. Then the browser starts.
3. **Run.** Heartbeats keep the lease alive. If the lease is lost anyway (an admin broke the lock, the device was
   cut off for longer than the lease), the browser is stopped.
4. **Close.** The browser is stopped, its data is packed and uploaded as the next version, and only then is the
   lock handed back. The hub stamps the *last closed* time at that moment, so a time you see always means "the
   data was safe on the hub".
5. **Next device.** The next opener finds the lock free, pulls the new version and carries on with the same
   cookies.

If something goes wrong in the middle, nothing is merged: see [Lost lock](#a-lost-lock-or-lease).

## Running a hub

The shortest path from nothing to a running hub with one team and two people:

```sh
export CITRO_HUB_DATA=/var/lib/citro-hub        # the data directory; also --data-dir DIR

citro-hub admin create-user --name anna --role owner     # creates the hub; prints anna's token ONCE
citro-hub admin create-team --name Acme --owner anna     # prints the team id
citro-hub admin create-user --name ben --team Acme       # ben is a member of Acme; prints ben's token ONCE

citro-hub serve --listen 127.0.0.1:8470                  # plain HTTP, see "Behind a TLS reverse proxy"
```

```console
$ curl -s http://127.0.0.1:8470/healthz
{"ok":true,"version":"…"}
```

Tokens go to standard output and every explanation goes to standard error, so a script can keep a token with
`TOKEN=$(citro-hub admin create-user --name carol --role viewer --team Acme)`.

### `citro-hub serve`

`--data-dir` is accepted before or after the subcommand. Every option has an environment variable.

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--data-dir DIR` | `CITRO_HUB_DATA` | `citro-hub-data` | state, hosted profile data and logs; created by the first `admin create-user` |
| `--listen ADDR` | `CITRO_HUB_LISTEN` | `127.0.0.1:8470` | address to listen on, plain HTTP |
| `--lease-ttl SECS` | `CITRO_HUB_LEASE_TTL` | `90` | how long a lock lives without a heartbeat |
| `--max-lease-ttl SECS` | `CITRO_HUB_MAX_LEASE_TTL` | `600` | the longest lease a client may ask for |
| `--max-data-mb MIB` | `CITRO_HUB_MAX_DATA_MB` | `512` | the largest hosted profile-data archive |
| `--keep-versions N` | `CITRO_HUB_KEEP_VERSIONS` | `5` | how many versions of a profile's data are kept |
| `--rate-limit PER_MIN` | `CITRO_HUB_RATE_LIMIT` | `600` | requests per minute and (address, user); `0` turns the limit off |
| `--real-ip-header HEADER` | `CITRO_HUB_REAL_IP_HEADER` | none | behind a reverse proxy: the header that carries the client address |
| `--log-max-mb MIB` | `CITRO_HUB_LOG_MAX_MB` | `20` | rotate a team's log file when it reaches this size |
| `--log-files N` | `CITRO_HUB_LOG_FILES` | `3` | log files kept per team (the current one plus the rotated ones) |

`RUST_LOG` sets the hub's own log level (default `info`); that log goes to standard error. `--max-data-mb`,
`--keep-versions`, `--log-max-mb` and `--log-files` are raised to 1 if you give less, and `--max-lease-ttl` is
raised to `--lease-ttl` if it is lower. The failed-login limit (10 failures in 60 seconds block an address for 60
seconds) is not an option; see [Security model](#security-model).

What `serve` does:

* It takes `serve.lock` in the data directory. A second `serve` on the same directory refuses to start.
* It checks the hosted data against the state before the first request, and repairs what a crash left behind
  (see [Data directory](#data-directory)).
* On a listen address that is not loopback it prints a warning: *listening on ADDR without TLS: anybody on the
  network path can read tokens*. Put a [TLS reverse proxy](#behind-a-tls-reverse-proxy) in front.
* On Ctrl-C or SIGTERM it stops accepting requests, gives busy connections 10 seconds to finish (an upload in
  progress, for instance), writes the state and exits.
* Errors are printed as `citro-hub: error: …` with exit code 1.

### Data directory

Directories are created with mode `0700` and files with mode `0600`.

```
<data-dir>/
├── state.json            users (with token hashes), teams, members, groups, profiles, locks
├── state.lock            held briefly by whoever changes state.json (the server, or `citro-hub admin`)
├── serve.lock            held for as long as a `citro-hub serve` runs on this directory
├── blobs/<profile_id>/   hosted profile data, one file per kept version: v0000000001.bin, v0000000002.bin, …
└── logs/
    ├── <team_id>.jsonl   one log per team, JSON lines; rotated to .jsonl.1, .jsonl.2, …
    └── _server.jsonl     what belongs to no team: failed logins, start-ups; server admins only
```

* **`state.json`** is the whole truth about accounts, teams and profiles. It is written atomically (temporary file,
  `fsync`, rename), and every change is on disk before the answer to the request goes out. All changes run one
  after another, which is what gives a profile exactly one lock winner. The one exception is a *heartbeat*: it
  only marks the state dirty, and the state is written within 2 seconds. A crash or `kill -9` can therefore
  lose at most about two seconds of lease time.
* **`blobs/`** holds the hosted data. An upload is streamed into a temporary file, hashed on the way, and renamed
  into place only when every check has passed, so a reader never sees half a file. Version numbers only grow and
  are never reused; deleting a profile deletes its directory.
* **Crash recovery** happens at the start of `serve`: leftover temporary uploads are removed; a data file newer
  than the recorded version (an upload that was never acknowledged) is dropped; a missing current file falls back
  to the newest older one; a data directory that is missing altogether resets that profile's data record; and a
  current file whose size no longer matches the record (edited by hand, a torn disk) is read again and published
  as a *new* version, so that no client mistakes it for the one it already has. Each of these is a warning in the
  hub's log.
* **`logs/`**: see [What is logged](#what-is-logged).

## Install on an Ubuntu server

Two scripts in [`deploy/`](../deploy) set the hub up the way this document describes it. They are for Ubuntu or
Debian on x86_64 with systemd; the second one also needs nginx and certbot. Run them as root, and read them first:
they are short plain bash (about 215 and 165 lines), and the comment at the top of each says what it touches.

```sh
# 1. The hub, on 127.0.0.1:8470, under its own user. Prints the owner's token once.
sudo bash install-hub.sh --owner anna --team Acme

# 2. Make an A record  hub.example.com -> this server  (DNS only), then put HTTPS in front of the hub:
sudo bash expose-hub.sh hub.example.com you@example.com
```

**`install-hub.sh`** creates this, and nothing else:

| What | Where | Why |
|---|---|---|
| a system user | `citro-hub`: no password, no login shell | the hub holds your team's cookies; no other account on the machine may read them |
| the data directory | `/var/lib/citro-hub`, mode `0700`, owned by `citro-hub` | [the layout](#data-directory) |
| the program | `/usr/local/bin/citro-hub`, owned by root | the service cannot rewrite itself. It is the GitHub release, checked against its `SHA256SUMS` **before** anything on the system changes |
| the service | `/etc/systemd/system/citro-hub.service` | sandboxed (the file system is read-only except the data directory, no new privileges, no capabilities, IP sockets only), restarts on failure, and **listens on 127.0.0.1 only** |
| `citro-hub-admin` | `/usr/local/sbin/` | `citro-hub admin …` run as the hub's own user; run as root it would write files the service cannot open |

The first run needs `--owner`, because a hub does not exist without a user; on a server that already has data,
`--owner` is refused. Without arguments the script **upgrades** to the newest release: same data, new program,
service restarted (`--version X.Y.Z` names another release). It does not touch nginx or the firewall.

**`expose-hub.sh HOSTNAME [EMAIL]`** adds one nginx file (`sites-available/citro-hub`, linked into `sites-enabled`),
one certificate, and the folder for the certificate check. The file is the [block above](#behind-a-tls-reverse-proxy)
plus a port-80 block that answers the check and redirects everything else to HTTPS. It changes no other nginx file,
refuses a name that nginx already serves, and when a step fails it puts its own file back as it was and reloads
nginx. The email is only needed for the first certificate on a server.

**Before you pick the hostname:**

* Its DNS record points at this server, and certificates are public (certificate transparency). Anyone who finds the
  name finds the address. If the server's address is meant to stay hidden behind a CDN, use a name that has nothing to
  do with your other domains.
* Keep Cloudflare's proxy **off** for it ("DNS only"). The proxy refuses uploads over 100 MB on the Free and Pro plans
  while the hub accepts up to 512 MiB (`--max-data-mb`), and through the proxy the hub would see Cloudflare's
  addresses instead of your team's, which is what the failed-login block counts.

**Then, for each teammate:**

```sh
sudo systemctl stop citro-hub; sudo citro-hub-admin create-user --name ben --team Acme; sudo systemctl start citro-hub
```

It prints ben's token once; send it over a private channel. `--team` needs the hub stopped
([why](#while-a-hub-is-serving)), which takes a second or two. Ben then runs, on each of his devices:

```sh
export CITRO_HUB_TOKEN=citro_…
citro hub login --url https://hub.example.com --device-name ben-laptop
```

The scripts do not set up [backups](#backups-and-restore): copy `/var/lib/citro-hub` as that section describes.

### Updating from the app

The Teams page of the desktop app has a **Sync server** card: the hub's version, the newest release and, for a hub
operator, an **Update hub** button. Pressed, the hub leaves a note (`upgrade-requested`) in its data directory;
`citro-hub-upgrade.path` (systemd) sees it and runs `/usr/local/sbin/citro-hub-upgrade --requested` as root, a copy
of `install-hub.sh` that installs the **newest** release only (checked against `SHA256SUMS`, never older than what
runs) and restarts the hub, the data kept. What happened is in `upgrade-result.json`, which the card shows, and in
`journalctl -u citro-hub-upgrade`. The hub itself cannot write outside its data directory: the note is all it can do.
A hub installed before this existed gets the helper by running `install-hub.sh` (no arguments) once.

## Users, teams and tokens

Everything here is `citro-hub admin …` on the data directory. `--data-dir` (or `CITRO_HUB_DATA`) says which one.

| Command | What it does |
|---|---|
| `admin create-user --name NAME [--role ROLE] [--team TEAM]` | add a user and print its token once. Creates the hub when the data directory is new |
| `admin create-team --name NAME [--owner USER]` | create a team and print its id. The owner defaults to the first active admin |
| `admin add-member --team TEAM --user USER [--role ROLE]` | put a user in a team, or change the role they have there. `TEAM` is an id or an exact name |
| `admin list-users` | name, role, status, creation time and teams (`Team:role`) of every user. Never a token |
| `admin list-teams` | id, name, number of profiles and members (`user:role`) of every team |
| `admin rotate-token --name NAME` | a new token for a user, printed once. The old one stops working at once |
| `admin revoke-token --name NAME` | cut a user off for good: the token stops working and the user's locks are released |

* **Names.** Letters, digits and `. _ - @`, at most 64 characters, unique when case is ignored.
* **`--role`** is `owner`, `admin`, `member` (the default) or `viewer`. `owner` and `admin` make a *hub operator*
  (a server admin: creates teams and is owner of every team). `member` and `viewer` make an ordinary account. With
  `--team` (or in `add-member`) the same word is also the role in that team.
* **The first user** of a new data directory must be an `owner` or `admin`, because someone has to create the
  teams. `--team` is refused on a new hub: create the user, then the team.
* **Tokens** are printed once and never stored; only their SHA-256 is. A lost token is replaced with
  `rotate-token`. A rotated token kills the old one at once, but the user's locks stay theirs: a lock belongs to
  the user, not to the token.
* **Revoking** is final: the name stays taken, `rotate-token` refuses a revoked user, and the last active admin
  cannot be revoked (*add another admin first*).
* **Audit.** What the admin commands do is logged as user `admin-cli` on device `offline`.

### While a hub is serving

The running server owns teams and members; users and tokens can change under it.

| Command | Hub serving? |
|---|---|
| `create-user` without `--team`, `rotate-token`, `revoke-token`, `list-users`, `list-teams` | works. The server notices the change on its next request. A new user without a team sees nothing yet |
| `create-user --team`, `create-team`, `add-member` | **refused**, nothing changes: *a citro-hub is serving DIR: teams and members belong to the running server while it is up. Stop it first, or use the HTTP API* |

So with a hub running, create the team and add the member over HTTP, with the token of a hub operator:

```sh
ADMIN=citro_XXXX
HUB=https://hub.example.com

curl -sS -X POST "$HUB/v1/teams" -H "Authorization: Bearer $ADMIN" \
     -H 'Content-Type: application/json' -d '{"name":"Acme"}'
# {"team":{"id":"t…","name":"Acme","created_at":…},"role":"owner"}

curl -sS -X POST "$HUB/v1/teams/TEAM_ID/members" -H "Authorization: Bearer $ADMIN" \
     -H 'Content-Type: application/json' -d '{"user_name":"ben","role":"member"}'
```

Creating a team over HTTP needs a hub operator; the creator becomes its first owner. Adding a member needs team
admin or owner, and the user must exist already (`create-user` first, which is fine while the hub runs).

## Behind a TLS reverse proxy

`citro-hub` speaks plain HTTP and has no TLS of its own, on purpose: a proxy renews certificates and sits in front
of everything else you run. Tokens travel in the `Authorization` header, so **never** expose the hub's own port.
Listen on the loopback address, let the proxy forward to it, and tell the hub which header carries the client
address:

```sh
citro-hub serve --listen 127.0.0.1:8470 --real-ip-header X-Forwarded-For
```

nginx:

```nginx
server {
    listen 443 ssl;
    server_name hub.example.com;
    ssl_certificate     /etc/letsencrypt/live/hub.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/hub.example.com/privkey.pem;

    client_max_body_size 600m;       # a little above --max-data-mb (default 512)
    proxy_request_buffering off;     # stream uploads to the hub instead of spooling them to disk first
    proxy_buffering off;             # the same for downloads
    proxy_read_timeout 300s;
    proxy_send_timeout 300s;

    location / {
        proxy_pass http://127.0.0.1:8470;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;   # replace whatever the client sent
    }
}
```

Caddy (it writes `X-Forwarded-For` itself):

```
hub.example.com {
    request_body {
        max_size 600MB
    }
    reverse_proxy 127.0.0.1:8470 {
        flush_interval -1
    }
}
```

The hub side of this is covered by the test suite and by the run below; the two snippets are the standard form for
those servers, so adapt them to yours. `GET /healthz` needs no token and is what a monitor should poll.

**Why `--real-ip-header` matters.** The hub counts failed logins per client address: ten failures within a minute
block that address for a minute. Behind a proxy every connection comes from the proxy itself, so without the
option ten wrong tokens from *anybody* would lock *everybody* out. A real run against a hub started with the
option, then without it:

```console
# --real-ip-header X-Forwarded-For
12 bad tokens from client 203.0.113.9 (the proxy says so): [401, 401, 401, 401, 401, 401, 401, 401, 401, 401, 429, 429]
anna's valid token: from 203.0.113.9 -> 429 (that address is blocked), from 203.0.113.10 -> 200, without the header (the proxy itself) -> 200

# no option: the header is ignored, so the peer, the proxy, is the one that gets blocked
WITHOUT the option, 12 bad tokens each claiming a different address: [401, 401, 401, 401, 401, 401, 401, 401, 401, 401, 429, 429]
anna (header ignored, the peer 127.0.0.1 is the one that is blocked) -> 429
```

* The hub reads the **last** entry of the header, and only if it is an IP address; otherwise it uses the peer. The
  last entry is the one your own proxy wrote, so a client cannot choose its address by sending its own header. With
  a chain of proxies, make the one next to the hub write the real client address.
* Turn the option on **only** when the hub is reachable only through the proxy (loopback, or a firewall rule).
  Otherwise anybody can send the header and pick any address, and the failed-login block protects nothing.
* The request rate limit (`--rate-limit`) counts per (address, user) the same way.

## Backups and restore

Everything is in the data directory ([layout](#data-directory)): `state.json`, `blobs/` and `logs/`. The two `*.lock`
files are empty lock files and need no backup.

**A backup of a running hub.** Copy `state.json` **first**, then `blobs/`, then `logs/`:

```sh
D=/var/lib/citro-hub  B=/backup/citro-hub-$(date +%F)
mkdir -p "$B" && cp -p "$D/state.json" "$B/" && cp -rp "$D/blobs" "$D/logs" "$B/"
```

The order matters: version numbers only grow, so the blobs you copy second contain every version the state you copied
first points to. What can still differ (an upload that finished in between, a version pruned in between) is exactly
what the start-up check repairs ([crash recovery](#data-directory)): a blob newer than the state is dropped with a
warning, a missing current blob falls back to the newest older one. If you would rather not think about it, stop the
hub for the copy, or snapshot the filesystem (LVM, ZFS, btrfs): a snapshot is atomic.

**The backup holds everybody's cookies and the hashes of every token.** Keep it encrypted and off the hub's disk.

**Restore.** Stop the hub, put the files back into an empty data directory, keep the modes (directories `0700`, files
`0600`), start it. A real run of exactly that: a backup taken while the hub was serving, the whole data directory
deleted, the files copied back.

```console
backup of the RUNNING hub (state.json first, then blobs/ and logs/):
   state.json 1374
   blobs/k855oji/v0000000001.bin 29
   blobs/k855oji/v0000000002.bin 5000
   logs/_server.jsonl 4541
   logs/tb9gxxj8g.jsonl 2258

$ citro-hub admin --data-dir <tmp>/restored list-users
NAME  ROLE    STATUS  CREATED               TEAMS
anna  admin   active  2026-11-16T23:15:16Z  Acme:owner
ben   member  active  2026-11-16T23:15:17Z  Acme:member

GET /v1/me
-> 200 {"user":{"id":"uve3sau0d","name":"anna","role":"admin","disabled":false,"created_at":1794870916},"teams":[{"team":{"id":"tb9gxxj8g","name":"Acme","created_at":1794870916},"role":"owner"}]}

after the restore: GET data -> 200, 5000 bytes, version 2, identical to what was uploaded: True
```

Tokens keep working (the hub keeps only their hashes, and those came back with `state.json`), so nobody has to log in
again. A restore returns every profile to the data it had when the backup was made; a device opens a profile on the
hub's copy and keeps any changes the hub never saw *aside*, never merged ([below](#a-lost-lock-or-lease)).

**Moving the hub to another server** is the same: stop, copy the directory, start. If the address changes, each
device runs `citro hub login --url NEW` again (same token).

## Connecting a device

### Log in

A hub operator gives you a token ([Users, teams and tokens](#users-teams-and-tokens)). On each device:

```console
$ export CITRO_HUB_TOKEN=citro_…          # prefer the environment: an argument shows up in `ps`
$ citro hub login --url https://hub.example.com --device-name anna-laptop
logged in to https://hub.example.com as anna in team Acme
this device is `anna-laptop`; bring the team's profiles here with: citro hub pull

$ citro hub status
hub        https://hub.example.com
team       Acme (tus6xob7q)
user       anna
device     anna-laptop (dev-ee3f9722736a287c0d5a3e35)
reachable  yes
profiles   0 from the hub, 0 open on this device
```

| Option | Meaning |
|---|---|
| `--url URL` (env `CITRO_HUB_URL`) | the hub's address |
| `--token TOKEN` (env `CITRO_HUB_TOKEN`) | your access token |
| `--device-name NAME` | how this device appears in lock messages and logs; default: its host name |
| `--team TEAM` | the team to use, by id or name; only needed when the account is in several (*this account is in several teams (A, B): pick one with --team*) |

One device (one `--home`) is logged in to **one hub and one team** at a time. `status` also prints
`waiting <id> has newer data than the hub: citro hub recover <id>` when something is still to go up.

What is kept where, all under `<home>/hub/`, directories `0700` and files `0600`:

| File | Content |
|---|---|
| `login.json` | the hub address, **your token**, the team and your name. Treat the home directory like `~/.ssh` |
| `device.json` | this device's id (random, made once) and name |
| `bound/<profile id>` | a marker per hub profile: the data version and SHA-256 last agreed with the hub, and whether the browser ran since. Its presence is what makes a profile *belong to the hub*: it is **never** launched without the hub's lock |
| `sessions/<profile id>` | the process that is opening or closing a profile right now (one per profile per device) |
| `tmp/` | archives in transit |

### Share a profile, pull the team's

```console
$ citro new shop-fr-1 --proxy socks5://u:pw@198.51.100.7:1080 --country fr --os linux --remark 'first one'
kjwqe0s
created profile 1 (shop-fr-1); open it with: citro open kjwqe0s

$ citro hub push 1
kpwl4wu
1 (shop-fr-1) is on the hub now: profile data uploaded (version 1); lock released

$ citro ls
NO  ID       NAME       GROUP      PROXY                       STATUS  HUB
1   kpwl4wu  shop-fr-1  Ungrouped  socks5://198.51.100.7:1080  -       hub
```

`push` puts a local profile on the hub; from then on it belongs to the team. Three things to know:

* **The hub gives it a new id** (`kjwqe0s` became `kpwl4wu`). The persona seed is pinned to the old id
  (`"fingerprint_config": {"seed": "kjwqe0s"}`), so the browser keeps its identity under the new one.
* **Serial numbers stay per device.** The `1` in `citro open 1` is this device's shorthand; the id is the same on
  every device.
* A group is created on the hub by name when it is missing. A profile that is open, or already on a hub, is refused.

On the other device:

```console
$ citro hub pull
pulled 1 profile(s) from Acme: 1 new, 0 already here
  + kpwl4wu  shop-fr-1
```

`pull` brings **settings only**: names, remarks, groups, proxies, last-opened and last-closed times. Cookies and
browser data come down when the profile is opened. The local copy shows secrets masked (`"proxy_password":
"********"`); the real values arrive only with the lock. A profile that was deleted on the hub is reported
(`! id name: not on the hub any more. It stays here; citro hub unbind <id> makes it a local profile`) and left alone.

### Automatic sharing, and what stays on one computer

With a hub connected, the desktop app shares every profile with the team by itself (its background sync runs every
few minutes): nobody has to press *Share*. Two switches keep things on one computer:

* a profile's **Keep this profile on this computer only** (profile editor -> Advanced -> Team);
* a group's **Keep this group on this computer only** (Groups -> Settings): none of its profiles are shared.

A profile that is open is shared after it is closed (its data goes up with it). From then on every open takes the
hub's lock (nobody else, not even you on another computer, can open it meanwhile) and every close uploads the
cookies, tabs and the rest of the browser data.

### Who may use a group's profiles

Each group has rules, set in its Settings by a team owner or admin and **enforced by the hub**:

| Setting | Choices |
|---|---|
| Who sees its profiles | everyone, members and up, admins and owners, owners |
| Who opens and edits them | members and up, admins and owners, owners |
| Also allowed | chosen members, whatever their role (a viewer among them still only looks) |

Someone a group hides its profiles from does not get them from the hub at all: not in their list, not by id, not
in the CSV export or the logs, and they cannot lock them, read their data or move a profile into the group. Someone
who may only look gets them without being able to open or edit them. Owners always see and open everything, so a
team cannot lock itself out. A hub older than these rules ignores them; the app then refuses to save the rules and
says the hub needs an update (`deploy/install-hub.sh` upgrades it).

A group also carries settings everybody's app applies: its start pages, its bookmarks and the **defaults for new
profiles in the group** (Groups -> Settings -> *Set defaults…*; a new profile made while the group is selected starts
with them instead of Settings' defaults).

### Open and close

`citro open`, the Local API and the desktop app all go through the same code, and the first thing it does for a hub
profile is take the lock. This is a real run with the real Clearcote kernel: two devices, one profile, one hub.

```console
$ citro open --headless 1      # on anna-laptop (stays in the foreground)

$ citro hub locks              # what anyone in the team sees
ID       NAME       USER  DEVICE       OPEN FOR  FREES IN
kwu90vt  shop-fr-1  anna  anna-laptop  1s        1m30s

$ citro open --headless 1      # on ben-desktop
error: 1 (shop-fr-1) is open on another device: anna on anna-laptop (for 1s); it opens here as soon as it is closed there
```

Ben tries again through the Local API of his own machine (`citro serve`), in all three forms:

```console
GET /api/v1/browser/start?user_id=kwu90vt
{"code":-1,"data":{},"msg":"the profile is open: anna on anna-laptop"}

GET /api/v1/browser/start?serial_number=1&headless=1
{"code":-1,"data":{},"msg":"the profile is open: anna on anna-laptop"}

POST /api/v2/browser-profile/start  {"profile_id": "kwu90vt", "headless": "1"}
{"code":-1,"data":{},"msg":"the profile is open: anna on anna-laptop"}
```

Ben is a member, so he cannot break the lock; only a team owner or admin can:

```console
$ citro hub unlock 1
error: forbidden: this needs a team owner or admin
```

An owner or admin can, for instance when the laptop that holds the lock is lost or its user is away. Here Anna, the team
owner, breaks the lock that her own laptop holds, from another device:

```console
$ citro hub unlock 1
unlocked 1: its browser stops at its next check and its unsaved changes are kept aside

$ citro hub locks
nothing is open on the hub
```

The browser on the laptop is stopped at its next heartbeat, whatever it had not uploaded is kept aside
([below](#a-lost-lock-or-lease)), and the profile is free for the next device.

Anna closes her browser (Ctrl-C in the window where `citro open` runs, or just close the browser). The data goes up
**first**, and only then is the lock handed back:

```console
$ citro open --headless 1      # on anna-laptop, what it printed until it ended
profile 1 (kwu90vt) is running
  pid        1174212
  selenium   127.0.0.1:44297
  puppeteer  ws://127.0.0.1:44297/devtools/browser/a30d3c7a-…
kwu90vt belongs to the team hub: keep this window open. Closing the browser (or Ctrl-C here) uploads its data and hands the profile back.
closed 1 (shop-fr-1): profile data uploaded (version 2); lock released

$ citro hub locks
nothing is open on the hub
```

Now Ben opens it, gets the cookies and data Anna left, and when he closes it the hub holds version 3.

* `citro open` of a hub profile **stays in the foreground**: it is the process that keeps the lock alive. A
  script that wants a background browser uses `citro serve`, which does the same.
* While the browser runs, a supervisor renews the lease every third of its lifetime. If the lease is lost anyway
  (an admin broke the lock, the machine was cut off for longer than the lease), **the browser is stopped**: the
  profile is not this device's any more.
* The hub stamps *last closed* only after the data is safe, so a time you see always means "the data is on the hub".
* The app and `citro serve` close their team browsers properly when they quit; `citro open` does so on Ctrl-C,
  SIGTERM and SIGHUP.
* If the upload cannot reach the hub, the close keeps trying for up to ten minutes (lease kept alive). After that the
  data stays on the device and goes up at the next close or with `citro hub recover PROFILE`.

### Locks, unlocking, editing, deleting

| Command | What it does |
|---|---|
| `citro hub locks [--json]` | who has which profile open; `(this device)` marks your own |
| `citro hub unlock PROFILE` | owner or admin only: break the lock. The browser stops at its next check and its unsaved changes are kept aside. The hub writes an audit line naming who broke whose lock |
| `citro edit PROFILE …` | for a hub profile the change is written through to the hub and refused while anybody has the profile open (`409 locked`). Only what changed is sent |
| `citro delete PROFILE… [--everywhere]` | a hub profile belongs to the whole team, so plain `delete` refuses; `--everywhere` deletes it on the hub for everybody (admin or owner; not while it is open) |
| `citro hub recover PROFILE` | finish what an interrupted session left: lock, upload the newer data, unlock |
| `citro hub unbind PROFILE` | detach: it stays here with its data as an ordinary local profile; nothing changes on the hub |
| `citro hub logout` | forget the login. Hub profiles stay but cannot be opened until you log in again or unbind them |

```console
$ citro delete 1
1: kpwl4wu belongs to the team hub: deleting it removes it for the whole team, not only here
error: not deleted: 1
```

### A lost lock or lease

Two devices must never end up with one cookie jar, so nothing is ever merged. When the lock is lost under a running
browser, or the hub's copy changed while this device held the profile, the hub's copy wins and what this device did
is kept: its data directory is renamed to `<home>/profiles/<id>/data.conflict-<unix seconds>-<4 characters>`, the
hub's log gets a warning, and the next open pulls the hub's copy. You can open that folder with a browser by hand,
or delete it.

| Situation | What happens |
|---|---|
| the heartbeat stops (laptop asleep, cable cut) | after the lease time the profile is free; if you wake up and nobody took it, the session carries on and uploads as usual; if somebody did, your changes are kept aside |
| an admin breaks the lock | the browser is stopped at its next check; changes kept aside |
| a browser outlives the process that opened it (crash of `citro`, the app) | the next `citro` or app start *adopts* it: takes the lock again and supervises it; if somebody else has the profile by then, the browser is stopped and its changes are kept aside |
| the hub is down when you close | the data stays here, marked newer; `citro hub status` says `waiting`; it goes up on the next close or `citro hub recover` |
| `recover` after the team moved on | refused, and what this device has is kept aside: a late recovery never overwrites newer work |

### The desktop app

The Teams page does all of the above with buttons: Connect, Pull now, Share with team, Force unlock, Disconnect.
See [GUI.md](GUI.md#teams).

## The HTTP contract

For anyone who writes a client or a script. Everything is JSON in UTF-8 except the profile data (raw bytes) and the
CSV export. Every route except `GET /healthz` is under `/v1` and needs `Authorization: Bearer <token>`.

| Header | Direction | Meaning |
|---|---|---|
| `Authorization: Bearer <token>` | request | the user's token (scheme case-insensitive) |
| `X-Citro-Lease: <lease>` | request | the proof of a lock, for everything that needs it |
| `X-Citro-Device: <name>` | request | optional; only used in log lines |
| `X-Citro-Data-Version`, `X-Citro-Data-Sha256`, `ETag` | response | version and hash of the data served |
| `X-Citro-Data-Sha256: <hex>` | request | optional on upload: the declared SHA-256, checked against what arrives |
| `Retry-After: <seconds>` | response | on every `429` |

Every response carries `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`.

### Endpoints

| Method and path | What it does | Needs |
|---|---|---|
| `GET /healthz` | `{"ok":true,"version":"0.1.1"}` | nothing |
| `GET /v1/me` | the user and their teams with the role in each | any token |
| `GET /v1/teams`, `POST /v1/teams` | list; create `{"name"}` (hub operator; the creator becomes owner) | token / operator |
| `GET /v1/teams/{team}/members`, `POST …` | list; add `{"user_name","role"}` | member / admin |
| `DELETE /v1/teams/{team}/members/{user}` | remove a member | admin (owner for owners) |
| `GET /v1/teams/{team}/groups`, `POST …` | list; create `{"name","remark"}` | viewer / member |
| `PATCH /v1/groups/{group}`, `DELETE …` | rename / delete (its profiles become ungrouped) | member / admin |
| `GET /v1/teams/{team}/profiles` | list, secrets masked. Query: `group`, `q`, `limit`, `offset` | viewer |
| `POST /v1/teams/{team}/profiles` | create (the body is the profile; the hub assigns `profile_id`, `serial_number`, times) | member |
| `POST /v1/teams/{team}/profiles/bulk-delete` | `{"ids":[…]}`, at most 1000 → `{"deleted":[…],"skipped":[{"id","reason"}]}` | admin |
| `GET /v1/teams/{team}/profiles.csv` | the CSV export. Query: `group`, `bom` (default on; `bom=0` off). Also at `…/export/profiles.csv` | viewer |
| `GET /v1/teams/{team}/logs` | the team's log. Query: `kind`, `profile`, `level` (comma list), `since`, `limit` | viewer |
| `GET /v1/profiles/{id}` | one profile; secrets only with the lease, or for an owner or admin | viewer |
| `PATCH /v1/profiles/{id}` | change fields: a JSON merge patch (RFC 7386); `null` deletes a key. The lease is required | lease holder |
| `DELETE /v1/profiles/{id}` | delete the profile and its data | admin, unlocked |
| `POST /v1/profiles/{id}/lock` | `{"device_id","device_name","ttl_secs"?,"ttl_ms"?}` → a lease | member |
| `POST /v1/profiles/{id}/heartbeat` | renew the lease | lease |
| `POST /v1/profiles/{id}/unlock` | `{"closed":true}` hands the lock back and stamps *last closed* | lease |
| `POST /v1/profiles/{id}/force-unlock` | break a lock → `{"ok":true,"was_locked":bool}`; audited | admin |
| `GET /v1/profiles/{id}/data` | download the data (`?version=N` for an older kept one) | lease |
| `PUT /v1/profiles/{id}/data` | upload the data, raw bytes | lease |
| `GET /v1/profiles/{id}/data/versions` | the kept versions, newest first: metadata only | member |
| `GET /v1/logs`, `POST /v1/logs` | read (`team`, `profile`, `kind`, `level`, `since`, `limit`); write `{"device_id","entries":[…]}`, at most 500, all or nothing | viewer / member |
| `GET /v1/admin/users`, `GET /v1/admin/logs` | every user; the server log | hub operator |

An unknown path is `404 no such endpoint`, a wrong method `405`. A request body larger than 1 MiB (except the data
upload) is `413`; one that does not arrive within 30 s is `408`.

### Locking and data, in order

```console
POST /v1/profiles/kg24kzz/lock   {"device_id": "anna-laptop-0001", "device_name": "anna-laptop"}
-> 200 {"lease":"ls_…","expires_at":1794869842,"ttl_secs":90,"ttl_ms":90000,"profile_id":"kg24kzz","max_data_bytes":536870912}

POST /v1/profiles/kg24kzz/lock   (ben, another device)
-> 409 {"error":{"code":"locked","holder":{"device_name":"anna-laptop","expires_at":1794869842,"since":1794869751,"user":"anna"},"message":"profile is open: anna on anna-laptop"}}

POST /v1/profiles/kg24kzz/lock   (anna again, a second device)
-> 409 {"error":{"code":"locked", … "message":"profile is open: anna on anna-laptop"}}

PUT /v1/profiles/kg24kzz/data?base_version=0      (with X-Citro-Lease)
-> 200 {"version":1,"size":2002,"sha256":"dabd78fb…16a1","updated_at":1794869751}

PUT /v1/profiles/kg24kzz/data?base_version=0      (the same again: out of date)
-> 409 {"error":{"code":"stale","current_version":1,"message":"base_version does not match the stored data (current version is 1)"}}

GET /v1/profiles/kg24kzz/data                     (ben, no lease)
-> 409 {"error":{"code":"locked", …}}

POST /v1/profiles/kg24kzz/force-unlock            (ben, a member)
-> 403 {"error":{"code":"forbidden","message":"this needs a team owner or admin"}}
```

* **A second lock request from the same device** takes over: the earlier lease is void at once (this is how a
  restarted app gets its profile back). A request from *another* device is refused, even for the same user.
* **Upload.** The lease, the preconditions and the size are decided *before* the body is read, so a refused upload
  costs nothing. `base_version=N` is optimistic concurrency (`409 stale` when the stored version is not `N`);
  `If-Match: "<sha256>"` / `If-None-Match: *` work as usual (`412`, code `stale`). A body that does not match
  `X-Citro-Data-Sha256` is `400` and nothing is stored. The new version is the current one plus one.
* **Download.** `204` when nothing is stored yet; `304` when `If-None-Match` names the stored hash; otherwise `200`
  with the bytes and the version and hash headers.
* **Reading is as exclusive as writing**: both need the live lease, so a second device cannot even read the cookies.

### Errors

Every failure has the same body: `{"error":{"code","message"}}`, plus `holder` (user, device name, since, expiry; never
a device id or a secret) when somebody else has the profile, and `current_version` when the data moved.

| Status | `code` | When |
|---|---|---|
| 400 | `invalid` | bad input; also a log batch with an unknown profile |
| 401 | `unauthorized` | missing, unknown or revoked token (with `WWW-Authenticate: Bearer`) |
| 403 | `forbidden` | the role in the team is too low |
| 403 | `lease_required` | the action needs `X-Citro-Lease` and nobody holds a live lock |
| 403 | `lease_invalid` | the lease is wrong, expired, or was taken over (names the new holder when there is one) |
| 404 | `not_found` | no such thing, **or** you are not allowed to know it exists |
| 405 | `invalid` | wrong method |
| 408 | `timeout` | the JSON body did not arrive within 30 s |
| 409 | `locked` | the profile is open elsewhere (`holder`) |
| 409 | `stale` | `base_version` does not match (`current_version`) |
| 412 | `stale` | an `If-Match` / `If-None-Match` precondition failed |
| 413 | `too_large` | body or data over its limit (`--max-data-mb` for the data) |
| 429 | `rate_limited` | failed-login block or request rate limit; see `Retry-After` |
| 500 | `internal` | storage trouble; the message says what, never a path or a secret |

Failed-login `429`: *too many failed logins; try again later*. Rate limit `429`: *too many requests from this user
and address; retry in N s*.

## What is logged

There are three kinds of record, and all of them are scrubbed.

* **Team logs**, one file per team, readable by every member of it (`GET /v1/teams/{team}/logs`, the app's Logs
  page). A record has `ts`, `kind`, `level` (`debug`, `info`, `warn`, `error`), `team_id`, `user`, `device_id`,
  `profile_id` and `message`. Two kinds:
  * `citroenition` (also spelt `citro`): what happened. The hub writes *lock granted*, *data pushed*, *data pulled*,
    *force-unlock by anna: lock of ben on ben-desktop on `shop-fr-1` broken*, *profiles exported as CSV*; the apps
    write *opened on anna-laptop (kernel 151…)*, *profile data uploaded: version 3 (412 files, 5120 KiB packed)*, *the
    lock was lost*, *quit: …*.
  * `browser`: at the end of every session the device sends the tail of the browser's own log (at most 64 KiB and 200
    lines, each cut at 500 characters).
* **The server log**, `logs/_server.jsonl`, for what no team owns: failed logins (*authentication failed from
  203.0.113.9*, *… too many failures, the address is blocked for a while*), start-ups. Hub operators read it with
  `GET /v1/admin/logs`.
* **The hub's own console log** on standard error (`RUST_LOG`, default `info`), including one access line per
  request: `ip`, user (`-` if not authenticated), method, **path without the query string**, status, milliseconds.
  Headers, query strings and bodies are never logged.

A device sends log lines with `POST /v1/logs`: at most 500 per request, all or nothing; a message is cut at 4000
characters; a timestamp that is missing or more than a day in the future is replaced by the hub's. Files are rotated
at `--log-max-mb` (default 20 MiB), `--log-files` files per team (default 3), mode `0600`. A failed log write never
fails a request.

**The scrubber** runs on the device before a line is sent and again on the hub before it is stored. It replaces
with `********` or `…_****`:

* token look-alikes (`citro_` or `ls_` followed by 32 or more hex digits);
* `user:password@` in any URL;
* `key=value`, `key: value` and `"key":"value"` pairs whose key says secret (`pass`, `pwd`, `secret`, `token`,
  `cookie`, `fakey`, `2fa`, `apikey`, `auth`, `bearer`, `credential`, `private`, `csrf`, `jwt`, `session_id`, `sid`,
  `otp`; `sessionid` too);
* `Authorization: Bearer …` and `Basic …`, and the value of `Cookie:` / `Set-Cookie:` lines.

## Security model

* **Tokens.** `citro_` plus 32 random bytes (64 hex digits). Printed once; the hub stores only a SHA-256 of it, so a
  stolen `state.json` does not contain a usable token. One token per user; rotating kills the old one at once.
* **Leases** (`ls_` plus 24 random bytes) are stored the same way, as hashes, and are bound to the user they were
  issued to.
* **The server decides.** The apps hold no authority: a modified client cannot open a locked profile, read its
  cookies, or write its data without the lease, which only one device can hold.
* **Secrets stay out of lists.** Passwords, 2-FA keys, cookies and proxy passwords are masked (`********`) in every
  list, in the CSV, and for anybody who does not hold the lock (owners and admins excepted). So are `user:pass@` in
  URLs and any field whose name says secret. An edit that sends the mask back keeps the stored secret.
* **Outsiders learn nothing.** A team you are not in answers `404`, exactly as a team that does not exist.
* **Failed logins** are counted per client address in memory: ten in a minute block that address for a minute
  (`429`, `Retry-After`), and a blocked address is refused before its token is even looked at. This is not
  configurable, and a restart forgets it. See [the reverse proxy](#behind-a-tls-reverse-proxy) for the address.
* **Rate limit.** A token bucket per (address, user), `--rate-limit` requests a minute (default 600, a burst of that
  many is fine). It runs *after* authentication, so a runaway client cannot starve the others; guessing tokens is
  stopped by the failed-login block instead.
* **At rest.** Directories `0700`, files `0600`, every file written atomically. The hosted data is **not**
  encrypted by the hub: it is your team's cookies in a `.tar.gz`. Whoever has the data directory has them, so protect
  the server like a password manager (full-disk encryption, its own user, encrypted backups).
* **On the wire.** The hub is plain HTTP by design: terminate TLS in front of it and listen on loopback. The hub
  warns at start-up when it listens elsewhere.
* **Uploads and unpacking.** Uploads are streamed, hashed and size-limited, and installed atomically. A device that
  downloads an archive trusts nothing in it: absolute paths, `..`, links and device nodes are refused, and the
  unpacked size is capped.
* **Audit.** Every lock, unlock, force-unlock, upload, download, delete and export is in the team log with who and
  from which device; what `citro-hub admin` does is logged as user `admin-cli` on device `offline`.

## Troubleshooting

| You see | Cause and what to do |
|---|---|
| `error: 1 (shop-fr-1) is open on another device: anna on anna-laptop …` / `409 locked` | someone has it open. Wait for them to close it, or a team owner or admin runs `citro hub unlock` |
| `forbidden: this needs a team owner or admin` | the role in the team is too low for that action; ask an owner or admin |
| `unauthorized: missing, unknown or revoked token` | the token is wrong, was rotated, or the user was revoked. Get a new one from the operator, `citro hub login` again |
| `429` and `Retry-After` | too many failed logins from this address: wait a minute. Behind a proxy with every user blocked at once: `--real-ip-header` is missing |
| `the lease is gone` / `lease_invalid` | the device was cut off for longer than the lease (default 90 s) or an admin broke the lock. The browser is stopped; what it did is kept in `data.conflict-…` |
| `the hub's data changed while this device held the profile` | the lock had been lost; the next open pulls the hub's copy and the old one is kept aside |
| `waiting <id> has newer data than the hub` in `citro hub status` | an upload did not go through (hub down at close). Run `citro hub recover <id>` |
| `this account is in several teams (A, B): pick one with --team` | add `--team Acme` to `citro hub login` |
| `this account is in no team yet: ask a hub admin to add you` | the operator must add you (`admin add-member`, or over HTTP while the hub runs) |
| `a citro-hub is serving DIR: teams and members belong to the running server while it is up` | `create-team`, `add-member` and `create-user --team` cannot run against a directory a hub is serving. Stop the hub, or use the HTTP calls in [Users, teams and tokens](#while-a-hub-is-serving) |
| `citro-hub: error: … serve.lock …` | another `citro-hub serve` already runs on that data directory |
| `listening on ADDR without TLS` | the hub is listening on a non-loopback address. Put a [TLS proxy](#behind-a-tls-reverse-proxy) in front |
| `413 too_large` on a close | the profile's data exceeds `--max-data-mb` (and your proxy's `client_max_body_size`). Raise both, or clear the profile's cache |
| the hub's log: *dropping the unacknowledged upload vN of profile X* | a crash cut an upload short; the hub dropped it at start-up. The device still has its copy and uploads again |
| the hub's log: *data of profile X is missing on disk; resetting its metadata* | the blob directory was deleted by hand. The next device to open it uploads its copy as version 1 |
| `this device is not logged in to a team hub: run citro hub login …` | you logged out (or this is a fresh home): `citro hub login` again, or `citro hub unbind` to turn the profile into a local one |
