#!/usr/bin/env bash
# install-hub.sh: put the Citroenition team hub on this server, under its own locked-down user.
#
#   bash install-hub.sh --owner NAME --team "TEAM NAME"    first install: creates the hub, its owner and a team
#   bash install-hub.sh                                    upgrade to the newest release; the data stays
#   bash install-hub.sh --version 0.1.7                    a specific release instead of the newest
#   bash install-hub.sh --requested                         what the "Update hub" button of the app runs (below)
#
# Run it as root (Ubuntu / Debian with systemd, x86_64). It creates, and nothing else:
#
#   user     citro-hub                  a system account: no password, no login shell, no SSH
#   data     /var/lib/citro-hub         mode 0700, owned by citro-hub: nobody else on this machine can read the cookies
#   binary   /usr/local/bin/citro-hub   owned by root, so the service cannot rewrite itself
#   service  /etc/systemd/system/citro-hub.service
#                                       sandboxed; listens on 127.0.0.1:8470 only, so it is NOT reachable from outside yet
#   command  /usr/local/sbin/citro-hub-admin
#                                       `citro-hub admin ...` run as the hub's own user (run as root it would write
#                                       files the service could not open)
#   upgrade  /usr/local/sbin/citro-hub-upgrade  (a copy of this script)
#            /etc/systemd/system/citro-hub-upgrade.{path,service}
#                                       "Update hub" in the app: the hub (a hub operator asked) writes
#                                       /var/lib/citro-hub/upgrade-requested; systemd then runs this script as root with
#                                       --requested, which installs the NEWEST release only (checked against
#                                       SHA256SUMS), never an older one, and writes upgrade-result.json for the app.
#
# It does not touch nginx, the firewall or any other user's files. expose-hub.sh puts the hub on HTTPS.
# The owner's token is printed ONCE at the end, to this terminal only. Copy it into a password manager.
set -euo pipefail

# DESTDIR installs under another prefix instead of / (the tests use it; leave it unset).
DESTDIR=${DESTDIR:-}
DATA=$DESTDIR/var/lib/citro-hub
BINDIR=$DESTDIR/usr/local/bin
SBINDIR=$DESTDIR/usr/local/sbin
UNIT=$DESTDIR/etc/systemd/system/citro-hub.service
UNITDIR=$DESTDIR/etc/systemd/system
WORK=""

say() { printf '\n== %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
usage: bash install-hub.sh [--owner NAME [--team "TEAM NAME"]] [--version X.Y.Z]

  --owner NAME   the first user, an owner of the hub. Required the first time, refused afterwards.
  --team  NAME   a team for that owner to start with.
  --version V    the release to install (default: the newest one).
USAGE
}

# latest_version: the newest published release, read from the redirect GitHub gives for .../releases/latest
# (a draft or a pre-release is never "latest"). Prints e.g. 0.1.7.
latest_version() {
  local url
  url=$(curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o /dev/null -w '%{url_effective}' https://github.com/afftroen/Citroenition/releases/latest) || return 1
  url=${url##*/}
  [[ $url =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || return 1
  printf '%s\n' "${BASH_REMATCH[1]}"
}

# fetch_release VERSION DIR: download the Linux archive and SHA256SUMS into DIR, check the checksum, unpack citro-hub.
# The result is DIR/citroenition-vVERSION-x86_64-linux/citro-hub.
fetch_release() {
  local version=$1 dir=$2 name base
  name=citroenition-v$version-x86_64-linux
  base=https://github.com/afftroen/Citroenition/releases/download/v$version
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$dir/$name.tar.gz" "$base/$name.tar.gz"
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$dir/SHA256SUMS" "$base/SHA256SUMS"
  (cd "$dir" && grep -F "  $name.tar.gz" SHA256SUMS | sha256sum -c -)
  tar -xzf "$dir/$name.tar.gz" -C "$dir" --no-same-owner "$name/citro-hub"
  [ -x "$dir/$name/citro-hub" ] || die "no citro-hub in $name.tar.gz"
}

unit_text() {
  cat <<'UNIT'
[Unit]
Description=Citroenition team hub (profile locks and hosted cookies)
Documentation=https://github.com/afftroen/Citroenition/blob/main/docs/HUB.md
After=network.target

[Service]
Type=simple
User=citro-hub
Group=citro-hub
Environment=CITRO_HUB_DATA=/var/lib/citro-hub
Environment=CITRO_HUB_LISTEN=127.0.0.1:8470
Environment=CITRO_HUB_REAL_IP_HEADER=X-Forwarded-For
ExecStart=/usr/local/bin/citro-hub serve
Restart=on-failure
RestartSec=3
TimeoutStopSec=20

# It may write its own data directory and nothing else; no home directories, no devices, no new privileges.
ReadWritePaths=/var/lib/citro-hub
UMask=0077
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
ProtectProc=invisible
ProcSubset=pid
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
UNIT
}

upgrade_units() {
  cat > "$UNITDIR/citro-hub-upgrade.path" <<'PATHUNIT'
[Unit]
Description=Citroenition team hub: an update was requested from the app

[Path]
PathExists=/var/lib/citro-hub/upgrade-requested
Unit=citro-hub-upgrade.service

[Install]
WantedBy=multi-user.target
PATHUNIT
  cat > "$UNITDIR/citro-hub-upgrade.service" <<'SVC'
[Unit]
Description=Citroenition team hub: install the newest release (asked for from the app)
After=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/citro-hub-upgrade --requested
SVC
  chmod 0644 "$UNITDIR/citro-hub-upgrade.path" "$UNITDIR/citro-hub-upgrade.service"
}

# result STATE MESSAGE: what the app shows after "Update hub" (readable by the hub, which serves it).
result() {
  local f=$DATA/upgrade-result.json m
  # plain text only: quotes, backslashes and control characters would break the JSON
  m=$(printf '%s' "$2" | tr -d '"\\' | tr -c '[:print:]' ' ')
  printf '{"state":"%s","message":"%s","at":%s}\n' "$1" "$m" "$(date +%s)" > "$f.tmp"
  chown citro-hub:citro-hub "$f.tmp" 2>/dev/null || true
  chmod 0600 "$f.tmp"
  mv -f "$f.tmp" "$f"
}

# ver_ge A B: A >= B as versions.
ver_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" = "$2" ]; }

wrapper_text() {
  cat <<'WRAPPER'
#!/bin/sh
# `citro-hub admin ...` as the hub's own user. Run as root it would create root-owned files the service cannot open.
# Start from / : the caller may be standing in a directory (like /root) that the hub's user is not allowed to enter.
cd / || exit 1
exec runuser -u citro-hub -- env CITRO_HUB_DATA=/var/lib/citro-hub /usr/local/bin/citro-hub admin "$@"
WRAPPER
}

main() {
  local version="" owner="" team="" c fresh="" token="" out="" work team_id="" requested="" current=""
  while [ $# -gt 0 ]; do
    case $1 in
      --version) version=${2:?--version needs a value}; shift 2 ;;
      --owner)   owner=${2:?--owner needs a name};      shift 2 ;;
      --team)    team=${2:?--team needs a name};        shift 2 ;;
      --requested) requested=1; shift ;;
      -h|--help) usage; exit 0 ;;
      *) usage >&2; die "unknown option: $1" ;;
    esac
  done

  [ "$(id -u)" = 0 ] || die "run as root"
  [ "$(uname -m)" = x86_64 ] || die "the release is built for x86_64 Linux; this machine is $(uname -m)"
  for c in curl sha256sum tar install useradd runuser systemctl; do
    command -v "$c" >/dev/null || die "missing command: $c"
  done

  [ -f "$DATA/state.json" ] || fresh=1
  if [ -n "$requested" ]; then
    # asked for by the hub: only ever the newest release, only on an existing hub, never older than what runs
    rm -f "$DATA/upgrade-requested"
    [ -z "$fresh" ] || die "no hub here"
    version=""
    result running "installing the newest release"
    trap 'result failed "the update did not finish: see journalctl -u citro-hub-upgrade"' ERR
    current=$("$BINDIR/citro-hub" --version 2>/dev/null | awk '{print $2}') || current=""
  fi
  if [ -n "$fresh" ] && [ -z "$owner" ]; then
    usage >&2
    die "this server has no hub yet: give its first user with --owner NAME (and a team with --team)"
  fi
  if [ -z "$fresh" ] && { [ -n "$owner" ] || [ -n "$team" ]; }; then
    die "the hub already has data in $DATA; add users and teams with: citro-hub-admin create-user / create-team"
  fi

  if [ -z "$version" ]; then
    version=$(latest_version) || die "could not read the newest release from GitHub; name one with --version X.Y.Z"
  fi
  [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "the version must look like 0.1.7"
  if [ -n "$requested" ] && [ -n "$current" ] && ver_ge "$current" "$version"; then
    result done "already on $current, the newest release"
    exit 0
  fi

  # download and check first: a bad download or a failed checksum must leave the system exactly as it was
  say "download v$version and check it against SHA256SUMS"
  WORK=$(mktemp -d)
  trap 'rm -rf "$WORK"' EXIT
  work=$WORK
  fetch_release "$version" "$work"

  say "user and data directory"
  if id citro-hub >/dev/null 2>&1; then
    echo "user citro-hub exists"
  else
    useradd --system --user-group --home-dir "$DATA" --shell /usr/sbin/nologin --comment "Citroenition team hub" citro-hub
  fi
  install -d -o citro-hub -g citro-hub -m 0700 "$DATA"

  say "install"
  install -m 0755 -o root -g root "$work/citroenition-v$version-x86_64-linux/citro-hub" "$BINDIR/citro-hub.new"
  mv -f "$BINDIR/citro-hub.new" "$BINDIR/citro-hub"
  wrapper_text > "$SBINDIR/citro-hub-admin"
  chmod 0755 "$SBINDIR/citro-hub-admin"
  unit_text > "$UNIT"
  chmod 0644 "$UNIT"
  # the "Update hub" button: this script as the upgrade helper, and the units that run it when the hub asks
  install -m 0755 -o root -g root "${BASH_SOURCE[0]}" "$SBINDIR/citro-hub-upgrade.new"
  mv -f "$SBINDIR/citro-hub-upgrade.new" "$SBINDIR/citro-hub-upgrade"
  upgrade_units

  if [ -n "$fresh" ]; then
    # `citro-hub serve` refuses an empty data directory, so the first user has to exist before the service starts.
    say "the first owner${team:+ and the team}"
    token=$("$SBINDIR/citro-hub-admin" create-user --name "$owner" --role owner)
    if [ -n "$team" ]; then
      team_id=$("$SBINDIR/citro-hub-admin" create-team --name "$team" --owner "$owner")
      echo "team \"$team\" has the id $team_id"
    fi
  fi

  say "start"
  systemctl daemon-reload
  systemctl enable citro-hub >/dev/null 2>&1
  systemctl enable --now citro-hub-upgrade.path >/dev/null 2>&1 || true
  systemctl restart citro-hub
  for _ in $(seq 1 40); do
    if out=$(curl -fsS --max-time 2 http://127.0.0.1:8470/healthz 2>/dev/null); then break; fi
    out=""
    sleep 0.5
  done
  if [ -z "$out" ]; then
    journalctl -u citro-hub -n 25 --no-pager >&2 || true
    die "the hub did not come up; its log is above"
  fi
  echo "the hub answers: $out"
  if [ -n "$requested" ]; then
    result done "updated to $version"
    exit 0
  fi

  say "done"
  cat <<EOF
the hub runs as user citro-hub, its data is in $DATA, and it listens on 127.0.0.1:8470 (this machine only).

  its log        journalctl -u citro-hub -f
  who is in      citro-hub-admin list-users           (never shows tokens)
  add a teammate systemctl stop citro-hub; citro-hub-admin create-user --name NAME --team "TEAM NAME"; systemctl start citro-hub
                 It prints NAME's token once. The hub has to be stopped for --team; a second or two is fine.
                 With the hub running you can still create a user without --team, and add the member over HTTPS: docs/HUB.md,
                 "While a hub is serving".
  lost a token   citro-hub-admin rotate-token --name NAME
  next           bash expose-hub.sh hub.your-domain.com you@example.com        (HTTPS, so the team can reach it)
EOF
  if [ -n "$token" ]; then
    printf '\nTOKEN of the owner %s. It is shown this once; copy it into a password manager now:\n\n    %s\n\n' "$owner" "$token"
  fi
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
