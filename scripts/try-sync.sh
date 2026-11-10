#!/usr/bin/env bash
# Tries cloud sync on this Mac: a sync server on 127.0.0.1 and two
# ClipboardX instances ("A" and "B") as two devices. Each has a data
# directory of its own under $TMPDIR; none touches your real history, vault
# or keychain (secret keys and tokens stay in memory: after restarting an
# instance, unlocking asks for the secret key again).
#
#   scripts/try-sync.sh        Ctrl-C stops everything.
set -euo pipefail

cd "$(dirname "$0")/.."
root="${TMPDIR:-/tmp}/clipboardx-try-sync"
port="${PORT:-8787}"

cargo build --workspace --bins
target_dir=$(cargo metadata --format-version 1 --no-deps |
  sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

rm -rf "$root"
mkdir -p "$root/server" "$root/a" "$root/b"
# The two windows should not hide each other when clicked.
for device in a b; do
  sqlite3 "$root/$device/clipboardx.db" \
    "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     INSERT INTO settings VALUES ('hide_on_blur', 'false');"
done

pids=()
cleanup() {
  kill "${pids[@]}" 2>/dev/null || true
  echo
  echo "stopped; test data in $root"
}
trap cleanup EXIT INT TERM

"$target_dir/debug/clipboardx-sync-server" --listen "127.0.0.1:$port" --data "$root/server" \
  > "$root/server.log" 2>&1 &
pids+=($!)
sleep 1
admin=$(grep -o 'admin code [A-Z0-9-]*' "$root/server.log" | head -1 | cut -d' ' -f3)

for device in a b; do
  CLIPBOARDX_DATA_DIR="$root/$device" CLIPBOARDX_NO_KEYCHAIN=1 CLIPBOARDX_NO_CAPTURE=1 \
    CLIPBOARDX_SHOW=1 "$target_dir/debug/clipboardx" > "$root/$device.log" 2>&1 &
  pids+=($!)
  sleep 2
done

cat <<EOF

Sync server: http://127.0.0.1:$port   (log: $root/server.log)
Admin code:  $admin   (valid 10 minutes)

Two ClipboardX windows are open on the right edge, one on top of the other:
drag one by its title bar. The menu bar has two icons, one per window.

  A: 密码本 › create a vault › 设置 › 云同步 › 开启
     address http://127.0.0.1:$port, the admin code above;
     note the secret key, confirm it, enter the master password.
  A: 设置 › 云同步 › 添加设备 › 配对码
  B: 设置 › 云同步 › 加入: the same address and the pairing code,
     then unlock with the master password and the secret key.
  Then add or edit an entry on one: it appears on the other within seconds.

Ctrl-C stops the server and both windows.
EOF
wait
