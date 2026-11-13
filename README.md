# obs-multi-rtmp-rust

A Rust reimplementation of [**obs-multi-rtmp**](https://github.com/sorayuki/obs-multi-rtmp),
the OBS Studio plugin by **SoraYuki** that streams to multiple destinations at the same time.

All credit for the original idea, design, user interface and translations goes to SoraYuki.
This project rewrites the plugin with its logic in Rust and a minimal Qt dock, keeps the original
behavior and configuration format, and adds an independent RTMP/RTMPS session engine.
It is not affiliated with or endorsed by the original author. If you find the original plugin
useful, please consider [supporting SoraYuki](https://paypal.me/sorayuki0).

## Features

Everything the original plugin does:

- A **Multiple output** dock with one row per target: enable checkbox, status dot,
  elapsed time and Start/Stop button, plus compact **Start all**, **Stop all** and
  **Edit targets** buttons in a fixed header.
- Protocols: **RTMP** (OBS `rtmp_output`), **SRT/RIST** (`ffmpeg_mpegts_muxer`) and
  **WebRTC (WHIP)** (`whip_output`).
- Per-target video/audio encoders, or reuse of the main OBS encoders ("same as OBS").
- Per-target scene (independent scene views), audio track selection, sync start/stop
  with the main OBS stream, and hotkeys.
- Reads and writes the same `obs-multi-rtmp.json` file in the OBS profile, so existing
  targets carry over.

New in this reimplementation:

- **RTMP/RTMPS (Rust session engine)** protocol. One target can have a **Live** and a
  **Backup** ingest (e.g. YouTube primary and backup) that receive identical packets,
  each with its own independent session:
  - own thread and bounded queue per session; a slow or dead destination never blocks
    OBS or the other destination;
  - congestion handling that drops P/B frames first, then whole older GOPs, and never
    drops sequence headers (resent on reconnect);
  - automatic reconnection with exponential backoff and jitter;
  - per-stream **pause / resume / reconnect** buttons and live **bitrate, FPS, uptime,
    reconnects, drops and queue size** in the dock;
  - per-stream network options: drop threshold, IP address family, bind IP,
    buffer size, retry backoff and maximum retries.
  - TLS via `rustls` (no OpenSSL), independent of OBS's own RTMP output.
- **Advanced menu** (⚙ next to *Edit targets*) with an option to make OBS's main
  *Start Streaming* button run only the encoders through a null output, so OBS itself
  connects nowhere while targets keep streaming. The original stream service is
  restored when the option is unchecked.
- Bug fixes over the C++ plugin, such as hotkey actions running on the UI thread and
  hotkey bindings being reloaded from the profile.

## Requirements

- Windows 10/11 x64, Linux x86_64, or macOS 12 or newer (Apple Silicon or Intel)
- OBS Studio 33 or newer (64-bit)

On Linux the plugin must be built from source (see [Building on Linux](#building-on-linux)).
The Flatpak version of OBS cannot load plugins installed this way; use OBS from your
distribution or the official PPA.

## Installation

This plugin replaces the original obs-multi-rtmp (same DLL name and same configuration file).
Close OBS before installing or removing it.

**Installer:** run `obs-multi-rtmp-rust-<version>-windows-x64-Installer.exe`. It installs to
`C:\ProgramData\obs-studio\plugins\obs-multi-rtmp`, replacing the original plugin if present.
Running it again offers to update or remove the plugin; removing it keeps your targets,
which live in the OBS profile (`obs-multi-rtmp.json`).

**Zip:** extract `obs-multi-rtmp-rust-<version>-windows-x64.zip` into the OBS installation
folder (for example `C:\Program Files\obs-studio`, or a portable OBS). It contains
`obs-plugins/64bit/obs-multi-rtmp.dll` and `data/obs-plugins/obs-multi-rtmp/locale/`.
Remove any copy in `C:\ProgramData\obs-studio\plugins\obs-multi-rtmp` so only one is loaded.

**Manual:** copy the build output:

| File | Destination |
|---|---|
| `obs-multi-rtmp.dll` (+ `.pdb`) | `C:\ProgramData\obs-studio\plugins\obs-multi-rtmp\bin\64bit\` |
| `plugin/data/locale/*.ini` | `C:\ProgramData\obs-studio\plugins\obs-multi-rtmp\data\locale\` |

`plugin/install.ps1` builds the plugin and performs these steps, keeping a copy of the
previously installed DLL.

**Linux:** `bash plugin/install.sh` builds the plugin and installs it for the current user in
`~/.config/obs-studio/plugins/obs-multi-rtmp` (`bin/64bit/obs-multi-rtmp.so` and
`data/locale/`), keeping a copy of the previously installed library. Alternatively, extract
the `obs-multi-rtmp-rust-<version>-linux-x86_64.tar.gz` archive made by `plugin/package.sh`
into `~/.config/obs-studio/plugins`.

**macOS:** run `obs-multi-rtmp-rust-<version>-macos-universal.pkg`. It installs
`obs-multi-rtmp.plugin` for the current user in
`~/Library/Application Support/obs-studio/plugins`. The installer is not signed by Apple,
so macOS blocks it the first time: open **System Settings → Privacy & Security** and click
**Open Anyway**, then run it again. Alternatively, extract the `.zip` into that folder and
remove the quarantine flag, otherwise macOS refuses to load the plugin:

```bash
xattr -r -d com.apple.quarantine ~/Library/Application\ Support/obs-studio/plugins/obs-multi-rtmp.plugin
```

## Building

Prerequisites:

- Rust (stable, MSVC toolchain) and Visual Studio 2022 with the C++ workload.
- OBS 33 headers and import libraries (`obs.lib`, `obs-frontend-api.lib`) and the Qt 6
  package from `obs-deps`. The easiest way to get them is to configure the original
  [obs-multi-rtmp](https://github.com/sorayuki/obs-multi-rtmp) (based on
  `obs-plugintemplate`), which downloads them into its `.deps` folder.

Environment variables read by `plugin/build.rs`:

- `OBS_DEPS_DIR` — folder containing OBS `include/` and `lib/`
  (default: `../obs-multi-rtmp/.deps` next to this repository).
- `OBS_QT_DIR` — Qt 6 folder (default: `<OBS_DEPS_DIR>/obs-deps-qt6-2026-10-23-x64`).

```bash
cargo build --release -p obs-multi-rtmp
```

The DLL is produced as `target/release/obs_multi_rtmp.dll`; install it renamed to
`obs-multi-rtmp.dll`.

### Building on Linux

Prerequisites: Rust (stable), a C++17 compiler, `pkg-config`, the OBS 33 and Qt 6
development files, and SIMDe (the OBS headers include it). `plugin/build.rs` finds OBS and
Qt with pkg-config (`libobs` and `Qt6Widgets`). On Ubuntu 26.04, with OBS from the official
PPA (which also installs the OBS headers; the PPA has only OBS 32 for Ubuntu 24.04):

```bash
sudo add-apt-repository ppa:obsproject/obs-studio
sudo apt install obs-studio qt6-base-dev libsimde-dev build-essential pkg-config
```

If the files are somewhere pkg-config cannot find, set `OBS_INCLUDE_DIR` (the folder with
`obs-module.h`), `OBS_LIB_DIR` and `OBS_QT_DIR` (a Qt 6 folder with `include/` and `lib/`).

```bash
cargo build --release -p obs-multi-rtmp
```

The library is produced as `target/release/libobs_multi_rtmp.so`; install it renamed to
`obs-multi-rtmp.so`, or use `plugin/install.sh`.

### Building on macOS

Prerequisites: Xcode, CMake and Rust. `bash plugin/package-macos.sh` prepares the OBS
development files the way the original plugin does (downloads the OBS sources, obs-deps and
Qt 6 for macOS into `.deps-macos/` and builds libobs and obs-frontend-api), builds the plugin
for Apple Silicon and Intel, and writes a universal `.pkg` and `.zip` to `dist/`.

The `macOS` GitHub Actions workflow runs the same script on every push and
uploads the packages as a build artifact.

Without further setup the plugin gets an ad-hoc signature and the installer is unsigned.
To sign and notarize with an Apple Developer account, set these repository secrets (the
same names as the original plugin's workflow):

| Secret | Contents |
|---|---|
| `MACOS_SIGNING_CERT` | Base64 of a `.p12` with the Developer ID Application and Installer certificates |
| `MACOS_SIGNING_CERT_PASSWORD` | Password of the `.p12` |
| `MACOS_SIGNING_APPLICATION_IDENTITY` | `Developer ID Application: <name> (<team id>)` |
| `MACOS_SIGNING_INSTALLER_IDENTITY` | `Developer ID Installer: <name> (<team id>)` |
| `MACOS_NOTARIZATION_USERNAME` | Apple ID used for notarization |
| `MACOS_NOTARIZATION_PASSWORD` | App-specific password of that Apple ID |
| `MACOS_NOTARIZATION_TEAM_ID` | Team ID |

### Packaging

`plugin/package.ps1` builds the release DLL and writes to `dist/`, like the original
plugin's packaging script:

- `obs-multi-rtmp-rust-<version>-windows-x64.zip` — OBS install-folder layout
- `obs-multi-rtmp-rust-<version>-windows-x64-Installer.exe` — NSIS installer (`installer.nsi`);
  requires [NSIS](https://nsis.sourceforge.io) (`makensis.exe`), otherwise it is skipped
- `obs-multi-rtmp-rust-<version>-windows-x64-pdb.zip` — debug symbols

On Linux, `bash plugin/package.sh` writes `dist/obs-multi-rtmp-rust-<version>-linux-x86_64.tar.gz`.
On macOS, `bash plugin/package-macos.sh` writes `obs-multi-rtmp-rust-<version>-macos-universal.pkg`
and `.zip`.

### Tests

```bash
cargo test -p obs-rtmp-sessions
```

The plugin tests load OBS libraries. On Windows, add the OBS `bin\64bit` folder to `PATH` first:

```powershell
$env:PATH = "C:\Program Files\obs-studio\bin\64bit;$env:PATH"
cargo test -p obs-multi-rtmp
```

## Project layout

| Path | Contents |
|---|---|
| `src/` | `obs-rtmp-sessions`: the RTMP/RTMPS session engine (queue, sessions, transport, FLV, C API) |
| `include/` | C header for the engine's C API |
| `tests/`, `examples/` | Engine tests (loopback RTMP server, congestion isolation, failover) and tools |
| `plugin/src/` | The OBS plugin in Rust: libobs FFI, configuration, targets, outputs, services, hotkeys |
| `plugin/native/` | Minimal Qt 6 dock and dialogs (C++), driven by a JSON view model from Rust |
| `plugin/data/locale/` | Translations inherited from the original plugin |
| `docs/behavior-spec.md` | Behavior specification of the original C++ plugin used as the reference for this rewrite |

## License

GPL-2.0-or-later, the same license as the original obs-multi-rtmp. See [LICENSE](LICENSE).

Bundles [nlohmann/json](https://github.com/nlohmann/json) (MIT) in `plugin/native/third_party/`.
