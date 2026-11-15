#!/usr/bin/env bash
# Builds the plugin on macOS as a universal (Apple Silicon + Intel) bundle and packages it:
#   dist/obs-multi-rtmp-rust-<version>-macos-universal.pkg  (installer, per user)
#   dist/obs-multi-rtmp-rust-<version>-macos-universal.zip  (obs-multi-rtmp.plugin)
#
# Dependencies are prepared like the original plugin's buildspec: the OBS sources,
# obs-deps and Qt 6 for macOS are downloaded (SHA-256 checked) into .deps-macos/, and
# libobs + obs-frontend-api are built from the OBS sources. Requires Xcode, CMake and Rust.
#
# Signing is optional. Without settings the bundle gets an ad-hoc signature (enough to run
# on Apple Silicon; Gatekeeper asks the user to allow the downloaded installer). Set:
#   CODESIGN_IDENT             "Developer ID Application: ..." to sign the plugin
#   CODESIGN_IDENT_INSTALLER   "Developer ID Installer: ..."   to sign the .pkg
#   NOTARIZE_APPLE_ID, NOTARIZE_TEAM_ID, NOTARIZE_PASSWORD     to notarize the .pkg
set -euo pipefail

OBS_VERSION="33.0.1"
OBS_SHA256="9174ee0e3677c92d0d060bdce4c5d44dfee9fd3afddfb6333f098fd029b8de2b"
DEPS_VERSION="2026-10-23"
DEPS_SHA256="6ded92c5d97f1d1b5effa07be13cc9f539826aa9c65013a15872328e39acffbf"
QT_SHA256="b216560cea8497207f9fc350c713716fb84aec6b973bb0ff8ac6add7734abad5"
DEPLOYMENT_TARGET="12.0"
BUNDLE_ID="io.github.jose-sanchez100.obs-multi-rtmp-rust"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$here/Cargo.toml" | head -n1)"
name="obs-multi-rtmp-rust-$version-macos-universal"
deps="${OBS_DEPS_DIR:-$root/.deps-macos}"
obs_deps="$deps/obs-deps-$DEPS_VERSION-universal"
qt="$deps/obs-deps-qt6-$DEPS_VERSION-universal"
dist="$root/dist"
stage="$root/target/package-macos"
plugin="$stage/obs-multi-rtmp.plugin"

fetch() { # url file sha256
    if [ ! -f "$deps/$2" ]; then
        echo "Downloading $1"
        curl -fsSL -o "$deps/$2.part" "$1"
        mv "$deps/$2.part" "$deps/$2"
    fi
    echo "$3  $deps/$2" | shasum -a 256 -c -
}

extract() { # file destination [tar options]
    if [ ! -d "$deps/$2" ]; then
        mkdir -p "$deps/$2"
        tar -xf "$deps/$1" -C "$deps/$2" "${@:3}"
    fi
}

setup_deps() {
    mkdir -p "$deps"
    local base="https://github.com/obsproject/obs-deps/releases/download/$DEPS_VERSION"
    fetch "$base/macos-deps-$DEPS_VERSION-universal.tar.xz" "macos-deps-$DEPS_VERSION-universal.tar.xz" "$DEPS_SHA256"
    fetch "$base/macos-deps-qt6-$DEPS_VERSION-universal.tar.xz" "macos-deps-qt6-$DEPS_VERSION-universal.tar.xz" "$QT_SHA256"
    fetch "https://github.com/obsproject/obs-studio/archive/refs/tags/$OBS_VERSION.tar.gz" "obs-studio-$OBS_VERSION.tar.gz" "$OBS_SHA256"
    extract "macos-deps-$DEPS_VERSION-universal.tar.xz" "obs-deps-$DEPS_VERSION-universal"
    extract "macos-deps-qt6-$DEPS_VERSION-universal.tar.xz" "obs-deps-qt6-$DEPS_VERSION-universal"
    extract "obs-studio-$OBS_VERSION.tar.gz" "obs-studio-$OBS_VERSION" --strip-components=1
    xattr -r -d com.apple.quarantine "$deps" 2>/dev/null || true

    # libobs and obs-frontend-api from the OBS sources (same options as the original plugin).
    local src="$deps/obs-studio-$OBS_VERSION"
    cmake -S "$src" -B "$src/build_universal" -G Xcode \
        "-DCMAKE_OSX_ARCHITECTURES=arm64;x86_64" \
        -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
        -DOBS_CMAKE_VERSION=3.0.0 -DENABLE_PLUGINS=OFF -DENABLE_FRONTEND=OFF \
        -DOBS_VERSION_OVERRIDE="$OBS_VERSION" \
        "-DCMAKE_PREFIX_PATH=$obs_deps;$qt"
    cmake --build "$src/build_universal" --target obs-frontend-api --config Release --parallel
    cmake --install "$src/build_universal" --component Development --config Release --prefix "$deps"
    touch "$deps/.ready-$OBS_VERSION-$DEPS_VERSION"
}

[ -f "$deps/.ready-$OBS_VERSION-$DEPS_VERSION" ] || setup_deps
echo "OBS development files:"
find "$deps/Frameworks" "$deps/lib" "$deps/include" -maxdepth 2 \( -name '*.framework' -o -name '*.dylib' -o -name 'obs-module.h' -o -name 'obs-frontend-api.h' \) 2>/dev/null || true

# The OBS headers include SIMDe (header-only). Use the copy in obs-deps when present.
if [ -z "${SIMDE_INCLUDE_DIR:-}" ] && ! ls "$deps"/*/include/simde/x86/sse2.h >/dev/null 2>&1; then
    if command -v brew >/dev/null 2>&1; then
        brew list simde >/dev/null 2>&1 || brew install simde
        export SIMDE_INCLUDE_DIR="$(brew --prefix)/include"
    fi
fi
echo "SIMDe: $(ls "$deps"/*/include/simde/x86/sse2.h 2>/dev/null || echo "${SIMDE_INCLUDE_DIR:-not found}")"

export OBS_DEPS_DIR="$deps" OBS_QT_DIR="$qt" MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"
targets=(aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${targets[@]}"
libs=()
for t in "${targets[@]}"; do
    (cd "$here" && cargo build --release -p obs-multi-rtmp --target "$t")
    libs+=("$root/target/$t/release/libobs_multi_rtmp.dylib")
done

# Bundle layout OBS expects: Contents/MacOS/<name> and Contents/Resources/ (module data).
rm -rf "$stage"
mkdir -p "$plugin/Contents/MacOS" "$plugin/Contents/Resources/locale" "$dist"
rm -f "$dist/$name"*  # other versions in dist/ are kept
lipo -create -output "$plugin/Contents/MacOS/obs-multi-rtmp" "${libs[@]}"
install_name_tool -id "@rpath/obs-multi-rtmp.plugin/Contents/MacOS/obs-multi-rtmp" "$plugin/Contents/MacOS/obs-multi-rtmp"
install -m 644 "$here"/data/locale/*.ini "$plugin/Contents/Resources/locale/"
cat > "$plugin/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleExecutable</key><string>obs-multi-rtmp</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>obs-multi-rtmp</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$DEPLOYMENT_TARGET</string>
</dict>
</plist>
EOF
lipo -info "$plugin/Contents/MacOS/obs-multi-rtmp"

ident="${CODESIGN_IDENT:--}"
if [ "$ident" = "-" ]; then
    codesign --force --sign - "$plugin"
else
    codesign --force --sign "$ident" --options runtime --timestamp "$plugin"
fi
codesign --verify --strict --verbose=2 "$plugin"

# Zip for manual install (ditto keeps the signature and bundle structure).
ditto -c -k --keepParent "$plugin" "$dist/$name.zip"

# Installer: installs into the user's ~/Library/Application Support/obs-studio/plugins.
pkgroot="$stage/pkgroot"
mkdir -p "$pkgroot/Library/Application Support/obs-studio/plugins"
ditto "$plugin" "$pkgroot/Library/Application Support/obs-studio/plugins/obs-multi-rtmp.plugin"
# Without this, Installer may "relocate" the bundle to another copy with the same identifier.
pkgbuild --analyze --root "$pkgroot" "$stage/components.plist"
plutil -replace 0.BundleIsRelocatable -bool NO "$stage/components.plist"
pkgbuild --identifier "$BUNDLE_ID" --version "$version" --root "$pkgroot" \
    --component-plist "$stage/components.plist" "$stage/obs-multi-rtmp.pkg"
cat > "$stage/distribution.xml" <<EOF
<?xml version="1.0" encoding="utf-8" standalone="no"?>
<installer-gui-script minSpecVersion="1.0">
    <options rootVolumeOnly="true" hostArchitectures="arm64,x86_64" customize="never" allow-external-scripts="no" />
    <domains enable_currentUserHome="true" enable_anywhere="false" enable_localSystem="false" />
    <title>obs-multi-rtmp (Rust) $version</title>
    <choices-outline><line choice="obs-plugin" /></choices-outline>
    <choice id="obs-plugin" title="obs-multi-rtmp"><pkg-ref id="$BUNDLE_ID" /></choice>
    <pkg-ref id="$BUNDLE_ID" version="$version">obs-multi-rtmp.pkg</pkg-ref>
    <volume-check><allowed-os-versions><os-version min="$DEPLOYMENT_TARGET" /></allowed-os-versions></volume-check>
</installer-gui-script>
EOF
pkg="$dist/$name.pkg"
productbuild --distribution "$stage/distribution.xml" --package-path "$stage" "$stage/unsigned.pkg"
if [ -n "${CODESIGN_IDENT_INSTALLER:-}" ]; then
    productsign --sign "$CODESIGN_IDENT_INSTALLER" "$stage/unsigned.pkg" "$pkg"
else
    cp "$stage/unsigned.pkg" "$pkg"
fi

if [ -n "${NOTARIZE_APPLE_ID:-}" ] && [ -n "${CODESIGN_IDENT_INSTALLER:-}" ]; then
    xcrun notarytool submit "$pkg" --apple-id "$NOTARIZE_APPLE_ID" --team-id "$NOTARIZE_TEAM_ID" \
        --password "$NOTARIZE_PASSWORD" --wait
    xcrun stapler staple "$pkg"
fi

echo "Created $pkg"
echo "Created $dist/$name.zip"
