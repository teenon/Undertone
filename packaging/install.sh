#!/usr/bin/env bash
# Install Undertone's user-level launchers, desktop entry, and systemd
# user unit into the standard XDG locations. Run from anywhere.
#
#   ./packaging/install.sh                 # install
#   ./packaging/install.sh --enable        # install AND enable+start the daemon
#   ./packaging/install.sh --uninstall     # remove everything
#
# The daemon binary is expected at $UNDERTONE_REPO/target/release/
# undertone-daemon (default $HOME/Undertone). Build it first with:
#   cargo build --release -p undertone-daemon
#   (cd undertone-tauri && cargo tauri build --no-bundle)
set -euo pipefail

REPO="${UNDERTONE_REPO:-$HOME/Undertone}"
PKG="$REPO/packaging"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

case "${1:-install}" in
    install|--install)
        mkdir -p "$BIN_DIR" "$APPS_DIR" "$SYSTEMD_DIR"
        install -m 0755 "$PKG/bin/undertone"      "$BIN_DIR/undertone"
        install -m 0755 "$PKG/bin/wave-mic-test"  "$BIN_DIR/wave-mic-test"
        install -m 0644 "$PKG/desktop/undertone.desktop" "$APPS_DIR/undertone.desktop"
        install -m 0644 "$PKG/systemd/undertone-daemon.service" "$SYSTEMD_DIR/undertone-daemon.service"
        systemctl --user daemon-reload 2>/dev/null || true
        update-desktop-database "$APPS_DIR" 2>/dev/null || true
        echo "Installed. Try:"
        echo "  undertone                                    # launch app (auto-starts daemon)"
        echo "  systemctl --user enable --now undertone-daemon  # auto-start daemon at login"
        ;;
    --enable|enable)
        systemctl --user daemon-reload 2>/dev/null || true
        systemctl --user enable --now undertone-daemon.service
        echo "Daemon enabled and started."
        systemctl --user status undertone-daemon.service --no-pager | head -5
        ;;
    --uninstall|uninstall)
        systemctl --user disable --now undertone-daemon.service 2>/dev/null || true
        rm -f "$BIN_DIR/undertone" "$BIN_DIR/wave-mic-test"
        rm -f "$APPS_DIR/undertone.desktop"
        rm -f "$SYSTEMD_DIR/undertone-daemon.service"
        systemctl --user daemon-reload 2>/dev/null || true
        update-desktop-database "$APPS_DIR" 2>/dev/null || true
        echo "Removed."
        ;;
    *)
        echo "Usage: $0 [install|--enable|--uninstall]" >&2
        exit 1
        ;;
esac
