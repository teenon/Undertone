# Packaging

User-level installation artifacts for Undertone — distribution-friendly
versions of the launcher script, desktop entry, systemd user unit, and
udev rules. No hardcoded paths; all use `$HOME` / `%h` / standard XDG
directories.

## Install

```sh
./packaging/install.sh --deps           # install system packages (apt/dnf/pacman, sudo)
./packaging/install.sh                  # copy launcher / desktop / systemd into ~/.local/
./packaging/install.sh --enable         # also enable + start the systemd unit
./packaging/install.sh --check          # report which deps are present vs missing
./packaging/install.sh --uninstall      # remove user-level files (deps untouched)
```

A typical first-run sequence on a fresh machine:

```sh
./packaging/install.sh --deps
cargo build --release -p undertone-daemon
(cd undertone-tauri && cargo tauri build --no-bundle)
./packaging/install.sh --enable
systemctl --user restart pipewire wireplumber pipewire-pulse  # one-time, loads the effects chain
```

After installing you can:

- Run `undertone` from a terminal — launches the daemon (if not already
  running) then opens the Tauri UI.
- Search "Undertone" in your application menu — same launcher behind the
  `.desktop` entry.
- Run `wave-mic-test` — quick 5 s record-and-playback through the Wave
  XLR's mic + headphones.

## Files

| Path                                      | Installs to                                          |
|-------------------------------------------|------------------------------------------------------|
| `bin/undertone`                           | `~/.local/bin/undertone`                             |
| `bin/wave-mic-test`                       | `~/.local/bin/wave-mic-test`                         |
| `desktop/undertone.desktop`               | `~/.local/share/applications/undertone.desktop`      |
| `systemd/undertone-daemon.service`        | `~/.config/systemd/user/undertone-daemon.service`    |
| `udev/70-undertone-elgato.rules`          | (manual sudo install — see file header)              |

The udev rule is **system-wide** and needs `sudo`; it lives here for
reference and is not handled by `install.sh`. Install it once with:

```sh
sudo cp packaging/udev/70-undertone-elgato.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --attr-match=idVendor=0fd9
```

## Mic effects (LV2/LADSPA plugins)

The Undertone mic chain (noise suppression, gate, compressor,
parametric EQ) lives in PipeWire and needs three system packages:

```sh
sudo apt install lsp-plugins-lv2 lilv-utils
```

`lsp-plugins-lv2` provides the gate, compressor, and parametric EQ;
`lilv-utils` is the LV2 host PipeWire uses internally.

**Noise suppression (RNNoise, optional, no apt package on Mint 22.3):**

```sh
git clone https://github.com/werman/noise-suppression-for-voice
cd noise-suppression-for-voice
mkdir build && cd build
cmake .. -DCMAKE_BUILD_TYPE=Release
make
sudo install -m 0755 ladspa/librnnoise_ladspa.so /usr/lib/ladspa/
```

If `librnnoise_ladspa.so` is absent, the noise-suppression slot in the
UI still renders but has no effect at runtime — the daemon writes its
control name into the chain, and PipeWire silently ignores the missing
plugin.

**One-time PipeWire restart after first daemon run:**

The daemon writes its filter-chain config drop-in to
`~/.config/pipewire/filter-chain.conf.d/50-undertone-mic.conf`.
PipeWire only loads filter-chain configs at start, so the very first
time the daemon runs you need:

```sh
systemctl --user restart pipewire wireplumber pipewire-pulse
```

After that, the chain stays loaded across reboots and runtime
parameter tweaks (sliders in the Effects panel) apply immediately
via `pw-cli set-param` — no further restart needed.

## Customisation

- **Repo location:** the launcher and systemd unit assume the repo lives
  at `$HOME/Undertone`. Override the launcher with `UNDERTONE_REPO=/path
  undertone`. Override the systemd unit's binary path with `systemctl
  --user edit undertone-daemon` and an `[Service] ExecStart=` override.
- **Daemon log level:** the systemd unit defaults to `RUST_LOG=undertone=info`.
  Bump to `debug` via `systemctl --user edit undertone-daemon`.

## Troubleshooting

### Sliders are disabled even though the header pill says "Connected"

Usually means the Tauri client connected to the IPC socket but the daemon
behind it never managed to claim the Wave XLR. The most common cause is a
**second `undertone-daemon` process** also bound to the device. The systemd
unit is the canonical owner; manual `cargo run` or stale background launches
will fight it for USB interface 3 and the IPC socket.

Check the daemon log on startup:

```sh
journalctl --user -u undertone-daemon -n 50 --no-pager
```

A line like `another undertone-daemon process appears to be running (pid 12345)`
means there's a duplicate. Kill it:

```sh
pgrep -fa undertone-daemon
kill <stale-pid>
systemctl --user restart undertone-daemon
```

### Wave XLR isn't detected after a reboot or replug

`snd_usb_audio` and the daemon race during USB enumeration; the daemon
self-heals via a periodic rescan, but if the device gets stuck (visible
because `arecord -l` doesn't list the Wave XLR while `/proc/asound/card1/`
exists with only `usbmixer`), recover with:

```sh
systemctl --user stop undertone-daemon
# unplug the Wave XLR USB cable, wait ~3 s, plug it back in
systemctl --user restart wireplumber
systemctl --user start undertone-daemon
```

### Quick mic sanity check

```sh
wave-mic-test            # records 5 s from Wave XLR mic, plays back through Wave XLR headphones
wave-mic-test 10         # 10 s instead
```

The script looks the source/sink up by prefix because PipeWire sometimes
appends a numeric suffix (e.g. `.3`) when the same device re-registers across
replugs / wireplumber restarts.

### Effects sliders move but nothing changes audibly

The PipeWire filter chain only loads at PipeWire start. After the very first
daemon run (which writes the drop-in config), do the one-time restart:

```sh
systemctl --user restart pipewire wireplumber pipewire-pulse
```

After that, slider tweaks apply live via `pw-cli set-param`.
