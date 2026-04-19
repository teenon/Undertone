# Packaging

User-level installation artifacts for Undertone — distribution-friendly
versions of the launcher script, desktop entry, systemd user unit, and
udev rules. No hardcoded paths; all use `$HOME` / `%h` / standard XDG
directories.

## Install

```sh
./packaging/install.sh                  # copy files to ~/.local/, ~/.config/
./packaging/install.sh --enable         # also enable + start the systemd unit
./packaging/install.sh --uninstall      # remove everything (clean uninstall)
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

## Customisation

- **Repo location:** the launcher and systemd unit assume the repo lives
  at `$HOME/Undertone`. Override the launcher with `UNDERTONE_REPO=/path
  undertone`. Override the systemd unit's binary path with `systemctl
  --user edit undertone-daemon` and an `[Service] ExecStart=` override.
- **Daemon log level:** the systemd unit defaults to `RUST_LOG=undertone=info`.
  Bump to `debug` via `systemctl --user edit undertone-daemon`.
