#!/usr/bin/env bash
# Installs the Linux kani-cli from a Kani release after checking its published SHA-256.
# Usage: install-kani-cli.sh <tag> <bin-dir>   (needs GH_TOKEN for gh)
set -euo pipefail
tag="$1" bin="$2"
asset=kani-cli-x86_64-unknown-linux-gnu.tar.xz
tmp="$(mktemp -d)"
gh release download "$tag" -R kani-app/kani -p "$asset" -p "$asset.sha256" -D "$tmp"
(cd "$tmp" && sha256sum -c "$asset.sha256")
mkdir -p "$bin"
tar -xJf "$tmp/$asset" -C "$tmp"
install -m 755 "$(find "$tmp" -type f -name kani-cli | head -n1)" "$bin/kani-cli"
"$bin/kani-cli" --help > /dev/null
