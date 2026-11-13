#!/usr/bin/env bash
# Builds the Rust plugin and installs it for the current user on Linux
# (~/.config/obs-studio/plugins/obs-multi-rtmp). Backs up the installed library
# before replacing it. OBS must be closed.
# Usage: bash plugin/install.sh [plugin-dir]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
plugin_dir="${1:-${XDG_CONFIG_HOME:-$HOME/.config}/obs-studio/plugins/obs-multi-rtmp}"
backup_dir="$root/install-backups"

if pgrep -x obs >/dev/null 2>&1; then
    echo "OBS is running: close it before installing." >&2
    exit 1
fi

(cd "$here" && cargo build --release -p obs-multi-rtmp)

bin="$plugin_dir/bin/64bit"
locale="$plugin_dir/data/locale"
mkdir -p "$bin" "$locale" "$backup_dir"

installed="$bin/obs-multi-rtmp.so"
if [ -f "$installed" ]; then
    hash="$(sha256sum "$installed" | cut -c1-8)"
    backup="$backup_dir/obs-multi-rtmp.previous.$hash.so.bak"
    cp "$installed" "$backup"
    echo "Backup of the previous library: $backup"
fi
# Copy to a temporary name and rename, so a library mapped by a running process is not overwritten in place.
install -m 755 "$root/target/release/libobs_multi_rtmp.so" "$installed.new"
mv -f "$installed.new" "$installed"
install -m 644 "$here"/data/locale/*.ini "$locale/"
echo "Installed $installed (Rust) SHA256 $(sha256sum "$installed" | cut -d' ' -f1)"
