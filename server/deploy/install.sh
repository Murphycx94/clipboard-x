#!/usr/bin/env bash
# Installs the ClipboardX sync server on this machine (a Debian/Ubuntu LXC)
# behind Caddy: the server listens on 127.0.0.1 only, Caddy serves it over
# HTTPS on the domain and keeps the domain on the home's address (DDNS).
#
#   sudo ./install.sh ./clipboardx-sync-server ./caddy
#
# The first run puts /etc/clipboardx-sync/caddy.env in place and stops: fill
# in the domain and the AccessKey there, then run it again. Running it with
# new binaries upgrades them; configuration and data stay.
set -euo pipefail

server="${1:?usage: sudo ./install.sh <clipboardx-sync-server> <caddy>}"
caddy="${2:?usage: sudo ./install.sh <clipboardx-sync-server> <caddy>}"
here="$(cd "$(dirname "$0")" && pwd)"
conf=/etc/clipboardx-sync
caddy_env="$conf/caddy.env"

if [[ $EUID -ne 0 ]]; then
  echo "run as root (sudo)" >&2
  exit 1
fi

# --- Caddy's settings: the domain and the DNS key -----------------------
install -d -m 0755 "$conf"
if [[ ! -f "$caddy_env" ]]; then
  install -m 0600 "$here/caddy.env.example" "$caddy_env"
  echo "Fill in $caddy_env (domain, port, AccessKey), then run this again." >&2
  exit 1
fi
chmod 0600 "$caddy_env"
(
  set -a
  # shellcheck source=/dev/null
  . "$caddy_env"
  for name in SYNC_ZONE SYNC_NAME SYNC_PORT ALIYUN_ACCESS_KEY_ID ALIYUN_ACCESS_KEY_SECRET; do
    if [[ -z "${!name:-}" ]]; then
      echo "$caddy_env: $name is empty" >&2
      exit 1
    fi
  done
  if [[ "$SYNC_ZONE" == example.com ]]; then
    echo "$caddy_env: SYNC_ZONE is still example.com" >&2
    exit 1
  fi
)

# --- The sync server, on this machine only ------------------------------
install -m 0755 "$server" /usr/local/bin/clipboardx-sync-server
id clipboardx-sync >/dev/null 2>&1 ||
  useradd --system --no-create-home --shell /usr/sbin/nologin clipboardx-sync
# Rewritten each time: an older install listened on its Tailscale address.
printf 'CLIPBOARDX_SYNC_LISTEN=127.0.0.1:8787\nRUST_LOG=info\n' > /etc/clipboardx-sync.env
chmod 0644 /etc/clipboardx-sync.env
install -m 0644 "$here/clipboardx-sync.service" /etc/systemd/system/clipboardx-sync.service

# --- Caddy: HTTPS and DDNS ---------------------------------------------
install -m 0755 "$caddy" /usr/local/bin/clipboardx-caddy
id clipboardx-caddy >/dev/null 2>&1 ||
  useradd --system --no-create-home --shell /usr/sbin/nologin clipboardx-caddy
install -m 0644 "$here/Caddyfile" "$conf/Caddyfile"
install -m 0644 "$here/clipboardx-caddy.service" /etc/systemd/system/clipboardx-caddy.service
(
  set -a
  # shellcheck source=/dev/null
  . "$caddy_env"
  /usr/local/bin/clipboardx-caddy validate --config "$conf/Caddyfile" --adapter caddyfile >/dev/null
)

systemctl daemon-reload
started="$(date '+%Y-%m-%d %H:%M:%S')"
systemctl enable clipboardx-sync clipboardx-caddy
# Restart, not start: an upgrade must run the new binaries.
systemctl restart clipboardx-sync clipboardx-caddy
sleep 2
systemctl --no-pager --lines=0 status clipboardx-sync clipboardx-caddy || true

# shellcheck source=/dev/null
. "$caddy_env"
lan="$(hostname -I | awk '{print $1}')"
echo
echo "Server address for ClipboardX:  https://$SYNC_NAME.$SYNC_ZONE:$SYNC_PORT"
echo "On the router: forward TCP $SYNC_PORT to $lan:$SYNC_PORT."
echo "The certificate and the DNS record take a minute: journalctl -u clipboardx-caddy -f"
echo "Admin code (first device only, valid 10 minutes after the first start):"
journalctl -u clipboardx-sync --since "$started" --no-pager | grep -o 'admin code [A-Z0-9-]*' | tail -1 ||
  echo "  none printed: a device is already registered"
