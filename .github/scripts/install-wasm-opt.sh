#!/usr/bin/env bash
# Installs wasm-opt from binaryen's own release, checked against a pinned SHA-256.
# Usage: install-wasm-opt.sh <bin-dir>   (needs GH_TOKEN for gh)
set -euo pipefail
version=version_133
sha256=2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489
asset="binaryen-$version-x86_64-linux.tar.gz"
tmp="$(mktemp -d)"
gh release download "$version" -R WebAssembly/binaryen -p "$asset" -D "$tmp"
echo "$sha256  $tmp/$asset" | sha256sum -c
tar -xzf "$tmp/$asset" -C "$tmp" "binaryen-$version/bin/wasm-opt"
mkdir -p "$1"
install -m 755 "$tmp/binaryen-$version/bin/wasm-opt" "$1/wasm-opt"
"$1/wasm-opt" --version
