#!/usr/bin/env bash
# Builds the Caddy the sync server sits behind, for Linux x86_64: HTTPS
# with certificates from Let's Encrypt by DNS challenge, and the domain's
# A record kept on the home's address (DDNS), both through Alibaba Cloud
# DNS. Needs Go.
#
#   server/deploy/build-caddy.sh OUT
set -euo pipefail

out="${1:?usage: build-caddy.sh <output path>}"

# Pinned: a rebuild is the same Caddy.
caddy_version=v2.11.9
xcaddy_version=v0.4.7
alidns_version=v1.0.29
dynamicdns_version=v0.0.0-20260805195708-67d107a42c02

gobin="$(mktemp -d)"
trap 'rm -rf "$gobin"' EXIT
GOBIN="$gobin" go install "github.com/caddyserver/xcaddy/cmd/xcaddy@$xcaddy_version"

GOOS=linux GOARCH=amd64 CGO_ENABLED=0 "$gobin/xcaddy" build "$caddy_version" \
  --output "$out" \
  --with "github.com/caddy-dns/alidns@$alidns_version" \
  --with "github.com/mholt/caddy-dynamicdns@$dynamicdns_version"
