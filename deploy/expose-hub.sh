#!/usr/bin/env bash
# expose-hub.sh: put the hub (127.0.0.1:8470) on https://HOSTNAME through the nginx that already runs on this server.
#
#   bash expose-hub.sh hub.example.com [you@example.com]
#
# Before it: install-hub.sh has run, and an A record  HOSTNAME -> this server's IP  exists (DNS only; see below).
# The email is needed only the first time certbot is used on this server.
#
# It adds ONE nginx file, /etc/nginx/sites-available/citro-hub (linked into sites-enabled), one certificate, and the
# folder /var/www/citro-hub-acme for the certificate check. It changes no other nginx file. If anything fails it
# puts its own file back the way it was and reloads nginx, so the sites that are already there keep working.
#
# Two things to decide before you pick the name:
#  * Certificates are public (certificate transparency), so the hostname becomes findable and it points at this server.
#    Choose one that has nothing to do with your landing or tracker domains.
#  * Leave the Cloudflare proxy (orange cloud) OFF for it ("DNS only"). Cloudflare refuses uploads over 100 MB on the
#    Free and Pro plans, and hosted profile data can be bigger than that.
set -euo pipefail

# DESTDIR installs under another prefix instead of / (the tests use it; leave it unset).
DESTDIR=${DESTDIR:-}
NGINX_DIR=$DESTDIR/etc/nginx
LE_DIR=$DESTDIR/etc/letsencrypt
SITE=$NGINX_DIR/sites-available/citro-hub
LINK=$NGINX_DIR/sites-enabled/citro-hub
ACME=$DESTDIR/var/www/citro-hub-acme
OK=""
TOUCHED=""

say() { printf '\n== %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# vhost_http HOST: port 80: answers the certificate check, sends everything else to https.
vhost_http() {
  cat <<'VHOST' | sed "s/__HOST__/$1/g"
server {
    listen 80;
    server_name __HOST__;
    location /.well-known/acme-challenge/ { root /var/www/citro-hub-acme; }
    location / { return 301 https://$host$request_uri; }
}
VHOST
}

# vhost_https HOST: port 443 with the certificate, forwarding to the hub. The proxy REPLACES X-Forwarded-For with the
# real client address, which is what the hub's failed-login block counts (see "Behind a TLS reverse proxy" in HUB.md).
vhost_https() {
  cat <<'VHOST' | sed "s/__HOST__/$1/g"
server {
    listen 443 ssl;
    server_name __HOST__;
    ssl_certificate     /etc/letsencrypt/live/__HOST__/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/__HOST__/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;

    client_max_body_size 600m;       # a little above the hub's --max-data-mb (512)
    proxy_request_buffering off;     # stream uploads to the hub instead of spooling them to disk first
    proxy_buffering off;
    proxy_read_timeout 300s;
    proxy_send_timeout 300s;

    location / {
        proxy_pass http://127.0.0.1:8470;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;   # replace whatever the client sent
    }
}
VHOST
}

# touch_site: called right before the first change to our nginx file; keeps a copy of what was there for the rollback.
touch_site() {
  [ -z "$TOUCHED" ] || return 0
  TOUCHED=1
  [ ! -f "$SITE" ] || cp -p "$SITE" "$SITE.prev"
}

# Runs when the script exits for any reason. After success (OK=1), or when nothing was changed yet, it does nothing; after a
# failure it puts our nginx file back the way it was and reloads nginx. OK and TOUCHED are global on purpose: a local of
# main() is already gone when the EXIT trap runs.
rollback() {
  [ -z "$OK" ] || return 0
  [ -n "$TOUCHED" ] || return 0
  echo "failed: putting nginx back the way it was" >&2
  if [ -f "$SITE.prev" ]; then mv -f "$SITE.prev" "$SITE"; else rm -f "$LINK" "$SITE"; fi
  if nginx -t >/dev/null 2>&1; then systemctl reload nginx || true; fi
}

main() {
  local host=${1:-} email=${2:-} c myip dnsip other host_re account
  local -a mail=()

  [ "$(id -u)" = 0 ] || die "run as root"
  [[ $host =~ ^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,}$ ]] || die "usage: bash expose-hub.sh hub.example.com [you@example.com]"
  for c in nginx certbot curl systemctl getent; do
    command -v "$c" >/dev/null || die "missing command: $c"
  done
  [ -d "$NGINX_DIR/sites-enabled" ] || die "this script expects the Debian/Ubuntu nginx layout (sites-available and sites-enabled)"
  curl -fsS --max-time 3 http://127.0.0.1:8470/healthz >/dev/null || die "the hub does not answer on 127.0.0.1:8470; run install-hub.sh first"

  say "DNS"
  dnsip=$(getent ahostsv4 "$host" | awk 'NR==1 {print $1}' || true)
  [ -n "$dnsip" ] || die "$host does not resolve yet: create the A record (DNS only), wait a minute, run this again"
  myip=$(curl -fsS --max-time 5 https://api.ipify.org 2>/dev/null || true)
  echo "$host -> $dnsip   (this server: ${myip:-unknown})"
  if [ -n "$myip" ] && [ "$dnsip" != "$myip" ]; then
    echo "warning: that is not this server. If $dnsip is Cloudflare (orange cloud), turn the proxy off for this name." >&2
  fi

  host_re=${host//./\\.}
  other=$(grep -rlE "server_name[^;]*[[:space:]]${host_re}[[:space:];]" "$NGINX_DIR" 2>/dev/null | grep -vxF "$SITE" || true)
  [ -z "$other" ] || die "nginx already has a server for $host in: $other"

  trap rollback EXIT
  install -d -m 0755 "$NGINX_DIR/sites-available"

  say "1/3  the certificate"
  if [ -s "$LE_DIR/live/$host/fullchain.pem" ]; then
    echo "already there"
  else
    account=$(find "$LE_DIR/accounts" -mindepth 3 -maxdepth 3 -type d 2>/dev/null | head -1 || true)
    if [ -z "$account" ]; then
      [ -n "$email" ] || die "the first certificate on this server needs an email: bash expose-hub.sh $host you@example.com"
      mail=(-m "$email")
    fi
    # a plain-HTTP block for this name first, so that the certificate authority can fetch its check file
    touch_site
    install -d -m 0755 "$ACME"
    vhost_http "$host" > "$SITE"
    ln -sf "$SITE" "$LINK"
    nginx -t
    systemctl reload nginx
    certbot certonly --webroot -w "$ACME" -d "$host" --non-interactive --agree-tos "${mail[@]}" \
      --deploy-hook "systemctl reload nginx"
  fi

  say "2/3  the HTTPS block"
  touch_site
  { vhost_http "$host"; echo; vhost_https "$host"; } > "$SITE"
  ln -sf "$SITE" "$LINK"
  nginx -t
  systemctl reload nginx

  say "3/3  check: the certificate chain, the proxy and the hub, from this machine"
  curl -fsS --max-time 10 --resolve "$host:443:127.0.0.1" "https://$host/healthz" || die "https://$host/healthz did not answer"
  echo
  OK=1
  rm -f "$SITE.prev"

  if ! systemctl is-enabled --quiet certbot.timer 2>/dev/null; then
    echo "warning: certbot.timer is not enabled, so the certificate will NOT renew itself. Run: systemctl enable --now certbot.timer" >&2
  fi
  cat <<EOF

done: https://$host
each teammate:   export CITRO_HUB_TOKEN=citro_...        (their own token, from: citro-hub-admin create-user ...)
                 citro hub login --url https://$host --device-name THEIR-DEVICE
EOF
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
