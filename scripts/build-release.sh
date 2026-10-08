#!/usr/bin/env bash
set -euo pipefail

version="${1:?Usage: scripts/build-release.sh <version>}"
package="packpub"
export PATH="$HOME/.cargo/bin:$PATH"
targets=(
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
  x86_64-apple-darwin
  aarch64-apple-darwin
)

command -v cargo-zigbuild >/dev/null || {
  echo "Install cargo-zigbuild first: cargo install --locked cargo-zigbuild" >&2
  exit 1
}
command -v zig >/dev/null || {
  echo "Install Zig first: https://ziglang.org/download/" >&2
  exit 1
}
command -v zip >/dev/null || {
  echo "zip command required" >&2
  exit 1
}

mkdir -p dist
for target in "${targets[@]}"; do
  rustup target add "$target"
  cargo zigbuild --release --target "$target"
  archive="dist/${package}-${version}-${target}.zip"
  rm -f "$archive"
  zip -j "$archive" "target/$target/release/$package"
  echo "Created $archive"
done
