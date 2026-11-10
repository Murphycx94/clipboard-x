#!/usr/bin/env bash
# Installs the ClipboardX sync server on this machine (a Debian/Ubuntu LXC
# with Tailscale up), listening on its Tailscale address only.
#
#   sudo ./install.sh ./clipboardx-sync-server [port]
set -euo pipefail

binary="${1:?usage: sudo ./install.sh <path to clipboardx-sync-server> [port]}"
port="${2:-8787}"
here="$(cd "$(dirname "$0")" && pwd)"

if [[ $EUID -ne 0 ]]; then
  echo "run as root (sudo)" >&2
  exit 1
fi
if ! command -v tailscale >/dev/null; then
  echo "Tailscale is not installed: see README.md, step 2" >&2
  exit 1
fi
address="$(tailscale ip -4 | head -1)"
if [[ -z "$address" ]]; then
  echo "Tailscale has no address: run 'tailscale up' first" >&2
  exit 1
fi

install -m 0755 "$binary" /usr/local/bin/clipboardx-sync-server
id clipboardx-sync >/dev/null 2>&1 ||
  useradd --system --no-create-home --shell /usr/sbin/nologin clipboardx-sync

if [[ ! -f /etc/clipboardx-sync.env ]]; then
  printf 'CLIPBOARDX_SYNC_LISTEN=%s:%s\nRUST_LOG=info\n' "$address" "$port" > /etc/clipboardx-sync.env
  chmod 0644 /etc/clipboardx-sync.env
fi

install -m 0644 "$here/clipboardx-sync.service" /etc/systemd/system/clipboardx-sync.service
systemctl daemon-reload
started="$(date '+%Y-%m-%d %H:%M:%S')"
systemctl enable clipboardx-sync
# Restart, not start: an upgrade must run the new binary.
systemctl restart clipboardx-sync
sleep 2
systemctl --no-pager --lines=0 status clipboardx-sync || true

echo
echo "Server address for ClipboardX:  http://$address:$port"
echo "Admin code (first device only, valid 10 minutes after the first start):"
journalctl -u clipboardx-sync --since "$started" --no-pager | grep -o 'admin code [A-Z0-9-]*' | tail -1 ||
  echo "  none printed: a device is already registered"
