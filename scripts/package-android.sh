#!/usr/bin/env bash
# Builds dist/ClipboardX-<version>-android.apk (arm64-v8a and armeabi-v7a)
# with `nsh package`, and its .sha256.
#
#   scripts/package-android.sh [--release]
#
# Signed with the release key when ANDROID_KEYSTORE (a keystore file) is
# set, with ANDROID_KEY_ALIAS and NSH_KEYSTORE_PASSWORD; otherwise with the
# debug key, for trying it out (it cannot update a release-signed install).
# --release refuses the debug key. Never commit the keystore: losing it, or
# its password, means no update can install over the released app.
#
# The version code (what Android compares on update) comes from the
# version: 1.2.3 is 10203.
set -euo pipefail

cd "$(dirname "$0")/.."

release=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) release=true; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if ! [[ "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "error: version '$version' is not major.minor.patch" >&2
  exit 1
fi
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
if ((minor > 99 || patch > 99)); then
  echo "error: version $version: minor and patch must be below 100 for the version code" >&2
  exit 1
fi
code=$((major * 10000 + minor * 100 + patch))

out=dist/android
args=(--target android --app-version "$version" --build "$code" --out "$out")
if [[ -n "${ANDROID_KEYSTORE:-}" ]]; then
  : "${ANDROID_KEY_ALIAS:?ANDROID_KEY_ALIAS goes with ANDROID_KEYSTORE}"
  : "${NSH_KEYSTORE_PASSWORD:?NSH_KEYSTORE_PASSWORD goes with ANDROID_KEYSTORE}"
  args+=(--keystore "$ANDROID_KEYSTORE" --key-alias "$ANDROID_KEY_ALIAS")
elif $release; then
  echo "error: --release needs ANDROID_KEYSTORE, ANDROID_KEY_ALIAS and NSH_KEYSTORE_PASSWORD" >&2
  exit 1
else
  echo "warning: no ANDROID_KEYSTORE: signing with the debug key" >&2
fi

rm -rf "$out"
nsh package "${args[@]}"

apk="dist/ClipboardX-$version-android.apk"
mv "$out/ClipboardX.apk" "$apk"
(cd dist && shasum -a 256 "$(basename "$apk")" > "$(basename "$apk").sha256")
echo "$apk (version code $code)"
