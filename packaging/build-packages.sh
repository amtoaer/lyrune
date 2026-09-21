#!/usr/bin/env bash
set -euo pipefail

export VERSION="${1:?usage: build-packages.sh VERSION ARCH DIST_DIR}"
export ARCH="${2:?missing architecture}"
dist_dir="$(realpath "${3:?missing dist directory}")"
case "$ARCH" in
  amd64|arm64) ;;
  *) echo "unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
export BINARY_DIR="$dist_dir/lyrune-v$VERSION-linux-$ARCH"
tar -C "$dist_dir" -xzf "$BINARY_DIR.tar.gz"
export GLIBC_VERSION
GLIBC_VERSION=$(readelf --version-info "$BINARY_DIR/lyrune" \
  | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' \
  | sed 's/GLIBC_//' | sort -Vu | tail -n 1)
test -n "$GLIBC_VERSION"

config_file=$(mktemp)
trap 'rm -f "$config_file"' EXIT
envsubst < packaging/nfpm.yaml > "$config_file"

nfpm package --config "$config_file" --packager deb \
  --target "$dist_dir/lyrune-v$VERSION-linux-$ARCH.deb"
nfpm package --config "$config_file" --packager rpm \
  --target "$dist_dir/lyrune-v$VERSION-linux-$ARCH.rpm"
nfpm package --config "$config_file" --packager archlinux \
  --target "$dist_dir/lyrune-v$VERSION-linux-$ARCH.pkg.tar.zst"
