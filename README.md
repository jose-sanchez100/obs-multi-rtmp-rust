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

- Windows 10/11 x64
- OBS Studio 33 or newer (64-bit)

## Installation

This plugin replaces the original obs-multi-rtmp (same DLL name and same configuration file).
Close OBS, uninstall or remove the original plugin, then copy the build output:

| File | Destination |
|---|---|
| `obs-multi-rtmp.dll` (+ `.pdb`) | `C:\ProgramData\obs-studio\plugins\obs-multi-rtmp\bin\64bit\` |
| `plugin/data/locale/*.ini` | `C:\ProgramData\obs-studio\plugins\obs-multi-rtmp\data\locale\` |

`plugin/install.ps1` builds the plugin and performs these steps, keeping a copy of the
previously installed DLL.

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

### Tests

```bash
cargo test -p obs-rtmp-sessions
```

The plugin tests load OBS libraries, so add the OBS `bin\64bit` folder to `PATH` first:

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
