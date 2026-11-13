#!/usr/bin/env bash
# Builds the release plugin and packages it for Linux x86_64:
#   dist/obs-multi-rtmp-rust-<version>-linux-x86_64.tar.gz
# The archive holds obs-multi-rtmp/{bin/64bit/obs-multi-rtmp.so,data/locale/*.ini};
# extract it into ~/.config/obs-studio/plugins.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$here/Cargo.toml" | head -n1)"
name="obs-multi-rtmp-rust-$version-linux-x86_64"
dist="$root/dist"
# Staging tree lives under target/ so dist/ only holds the deliverables.
stage_root="$root/target/package-linux"
stage="$stage_root/obs-multi-rtmp"

(cd "$here" && cargo build --release -p obs-multi-rtmp)

rm -rf "$stage_root"
mkdir -p "$stage/bin/64bit" "$stage/data/locale" "$dist"
rm -f "$dist"/obs-multi-rtmp-rust-*-linux-x86_64*
install -m 755 "$root/target/release/libobs_multi_rtmp.so" "$stage/bin/64bit/obs-multi-rtmp.so"
install -m 644 "$here"/data/locale/*.ini "$stage/data/locale/"

tar -C "$stage_root" --owner=0 --group=0 -czf "$dist/$name.tar.gz" obs-multi-rtmp
echo "Created $dist/$name.tar.gz"
