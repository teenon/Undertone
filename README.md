# Undertone

**Linux-native audio mixer for Elgato Wave devices** — providing Wave Link-style functionality using PipeWire.

Undertone gives you independent control over multiple audio channels with separate stream and monitor mixes, perfect for streamers and content creators on Linux.

> [!IMPORTANT]
> This project is primarily intended for AI experimentation and research. While functional, the codebase may emphasize exploration and iteration over refinement, and some components may be experimental or evolve rapidly. It is provided as-is and is not optimized for production use.

## What this fork adds

This fork ([teenon/Undertone](https://github.com/teenon/Undertone)) extends upstream
[polariscli/Undertone](https://github.com/polariscli/Undertone) with:

- **Elgato Wave XLR support** (VID:PID `0fd9:007d`) — mic gain, mute, and headphone
  volume via the device's vendor-specific USB control protocol (interface 3).
  Decoded protocol notes live in `protocol.md`.
- **Device trait abstraction** (`crates/undertone-hid/src/device_trait.rs`) — a
  `Device` trait + `DeviceModel` enum so future Elgato hardware can be added
  without forking the daemon. `Wave3` and `WaveXLR` are the two implementations.
- **Tauri desktop UI** (`undertone-tauri/`) — React + TypeScript + Tailwind, with
  a system tray (close-to-tray, right-click → Quit), auto-reconnect on daemon
  restart, and a loading-state for sliders so they no longer flash to 0% on
  launch. Replaces the Qt6/QML UI for day-to-day use; the Qt UI still builds.
- **Wave Link-style effects rack** (`crates/undertone-effects/`) — noise
  suppression (RNNoise), gate, compressor, and parametric EQ via PipeWire's
  native `module-filter-chain` and LSP LV2 plugins. Presets:
  Off / Voice / Streaming / Singing.
- **Packaging** (`packaging/`) — `install.sh` with `--deps`, `--check`,
  `--enable`, `--uninstall` modes; templated systemd user unit, `.desktop`
  entry, launcher script, and a `wave-mic-test` quick-record helper.

For the upstream feature set (5 virtual channels, app routing, profiles, Stream
vs. Monitor mixes), see the sections below.

## Features

- **5 Audio Channels** - System, Voice, Music, Browser, Game
- **Dual Mix Architecture** - Separate Stream and Monitor mixes with independent volume/mute per channel
- **Automatic App Routing** - Apps route to channels based on configurable rules (Discord → Voice, Spotify → Music, etc.)
- **Master Volume Control** - Per-mix master volume and mute
- **Output Device Selection** - Route monitor mix to any audio output (headphones, speakers, HDMI)
- **Profiles** - Save and load mixer configurations
- **Mic Control** - Gain, mute, and (Wave XLR) headphone volume
- **Effects Rack** - Noise suppression, gate, compressor, parametric EQ on the mic chain
- **Tauri Desktop UI** - React/TypeScript with a system tray icon (this fork)
- **Native Qt UI** - Qt6/QML with KDE Kirigami theming (upstream)

## Screenshots

_Coming soon_

## Requirements

- Linux with PipeWire (Fedora 43+, Ubuntu 24.04+, Mint 22.x, Arch, etc.)
- Elgato Wave:3 or Wave XLR microphone (optional — works as a general audio mixer too)
- Rust 1.85+ (Edition 2024)
- For the Tauri UI: WebKitGTK 4.1, libayatana-appindicator
- For the Qt UI (optional): Qt6 with Kirigami

## Installation

### Tauri UI (this fork's recommended path)

```sh
git clone https://github.com/teenon/Undertone.git
cd Undertone
./packaging/install.sh --deps             # apt/dnf/pacman, sudo
cargo build --release -p undertone-daemon
(cd undertone-tauri && cargo tauri build --no-bundle)
./packaging/install.sh --enable           # copies launcher + .desktop, enables systemd unit
```

After the first run, do a one-time PipeWire restart so the mic effects chain
loads from `~/.config/pipewire/filter-chain.conf.d/`:

```sh
systemctl --user restart pipewire wireplumber pipewire-pulse
```

Launch from your application menu ("Undertone") or run `undertone` in a terminal.
See [`packaging/README.md`](packaging/README.md) for `--check`, `--uninstall`,
the udev rule for non-root USB access, and the optional RNNoise build.

### Upstream installer (Qt UI)

The upstream `scripts/install.sh` still works for the Qt6/QML UI:

```bash
curl -sSL https://raw.githubusercontent.com/polariscli/Undertone/main/scripts/install.sh | bash
```

Manual Qt-UI dependencies:

```bash
# Fedora
sudo dnf install pipewire-devel qt6-qtbase-devel qt6-qtdeclarative-devel \
    clang kf6-kirigami-devel kf6-qqc2-desktop-style

# Arch Linux
sudo pacman -S pipewire qt6-base qt6-declarative clang kirigami

# Ubuntu/Debian
sudo apt install libpipewire-0.3-dev qt6-base-dev qt6-declarative-dev \
    clang libkf6kirigami-dev
```

You also need Rust 1.85+ (for Edition 2024):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Manual Build

```bash
git clone https://github.com/teenon/Undertone.git
cd Undertone
cargo build --release
```

### Run (Development)

```bash
# Start the daemon (required)
cargo run -p undertone-daemon --release

# In another terminal, start the Tauri UI (this fork)
(cd undertone-tauri && cargo tauri dev)

# Or the Qt UI (upstream)
cargo run -p undertone-ui --release
```

### Upstream install script

The upstream installer remains available for the Qt UI build path:

```bash
# Full installation
./scripts/install.sh install

# Uninstall completely
./scripts/install.sh uninstall

# Update to latest version
./scripts/install.sh update

# Check dependencies only
./scripts/install.sh check

# Service management
./scripts/install.sh start|stop|enable|disable|status|logs

# Install individual components
./scripts/install.sh udev|wireplumber|service|build
```

## How It Works

Undertone creates virtual audio sinks in PipeWire that applications connect to. Each channel feeds into volume filter nodes that control the audio level independently for Stream and Monitor mixes.

```
App (Spotify)
  -> ut-ch-music (channel sink)
       -> ut-ch-music-stream-vol -> ut-stream-mix -> OBS
       -> ut-ch-music-monitor-vol -> ut-monitor-mix -> Headphones
```

### Default App Routing

| Pattern   | Channel |
| --------- | ------- |
| discord   | Voice   |
| zoom      | Voice   |
| teams     | Voice   |
| spotify   | Music   |
| rhythmbox | Music   |
| firefox   | Browser |
| chrome    | Browser |
| steam     | Game    |
| _default_ | System  |

## Usage

### Mixer Tab

- Adjust volume sliders to control audio levels
- Click mute button to silence a channel
- Toggle between Stream and Monitor mix views
- Use master volume for overall mix control

### Apps Tab

- View currently playing audio applications
- Click channel dropdown to reassign apps
- Routes are automatically saved

### Device Tab

- View Wave:3 / Wave XLR connection status
- Adjust microphone gain
- Toggle mic mute
- Adjust headphone volume (Wave XLR)

### Effects Panel (Tauri UI)

- Per-effect bypass toggles for noise suppression, gate, compressor, EQ
- Per-parameter sliders (threshold, ratio, attack/release, EQ gain/freq/Q)
- Preset selector: Off, Voice, Streaming, Singing
- Reset chain to defaults

### System Tray (Tauri UI)

- Closing the window hides to tray instead of quitting; daemon keeps running so
  hardware state and effects are preserved.
- Left-click the tray icon to show/hide the window.
- Right-click → Quit fully exits the app (the daemon continues running under
  systemd unless stopped explicitly).

### Profiles

- Click profile name in header to switch profiles
- Use menu to save current settings or delete profiles

## Configuration

Data is stored in `~/.local/share/undertone/`:

- `undertone.db` - SQLite database with channels, routes, profiles
- Logs via systemd journal when running as service

WirePlumber configuration for Wave:3 naming:

- `~/.config/wireplumber/wireplumber.conf.d/51-elgato.conf`

## Troubleshooting

### No audio from channels

```bash
# Check if daemon is running
pgrep undertone-daemon

# Verify PipeWire nodes exist
pw-cli list-objects Node | grep ut-

# Check audio links
pw-link -l | grep ut-
```

### App routing to wrong channel

```bash
# Check database routes
sqlite3 ~/.local/share/undertone/undertone.db "SELECT * FROM app_routes;"

# Restart daemon to re-apply routes
pkill undertone-daemon && cargo run -p undertone-daemon
```

### UI not connecting

```bash
# Check socket exists
ls -la $XDG_RUNTIME_DIR/undertone/daemon.sock

# Test IPC
echo '{"id":1,"method":{"type":"GetState"}}' | \
    socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/undertone/daemon.sock
```

## Architecture

**undertone-daemon** (background service)

- IPC Server (Unix socket) | Signal Handler | Event Loop (Tokio)
- **undertone-core**: Channels, Mixer, App Routing, Profiles, State
- **undertone-pipewire**: PipeWire graph management + filter-chain config
- **undertone-db**: SQLite persistence
- **undertone-hid**: `Device` trait + Wave:3 / Wave XLR implementations
- **undertone-effects**: Mic effect chain (RNNoise / LSP gate, comp, EQ)

_Unix Socket IPC_

**undertone-tauri** (Tauri 2 + React + TypeScript + Tailwind) — this fork
**undertone-ui** (Qt6/QML + Kirigami + cxx-qt) — upstream

## Contributing

Contributions welcome! Please see [PROGRESS.md](PROGRESS.md) for current status and planned features.

## License

GPL-3.0 - See [LICENSE](LICENSE) for details.

## Acknowledgments

- [pipewire-rs](https://gitlab.freedesktop.org/pipewire/pipewire-rs) - Rust bindings for PipeWire
- [cxx-qt](https://github.com/KDAB/cxx-qt) - Safe Rust/Qt interop
- [KDE Kirigami](https://develop.kde.org/frameworks/kirigami/) - UI framework
- Elgato for the excellent Wave:3 hardware
