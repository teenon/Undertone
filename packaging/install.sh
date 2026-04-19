#!/usr/bin/env bash
# Install Undertone's user-level launchers, desktop entry, systemd
# user unit, and (optionally) the system packages we depend on.
#
#   ./packaging/install.sh                 # install user-level files
#   ./packaging/install.sh --enable        # install AND enable+start the daemon
#   ./packaging/install.sh --deps          # apt/dnf/pacman install of deps
#   ./packaging/install.sh --check         # report what's installed vs missing
#   ./packaging/install.sh --uninstall     # remove user-level files
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

# --- Dependency tables -------------------------------------------------
#
# The daemon needs PipeWire dev headers and rusb to talk to the device.
# The Tauri app needs WebKit2GTK and a few X11/Wayland helpers. The
# effects rack needs the LSP plugin LV2 bundle plus the lilv host.
# RNNoise is intentionally NOT in this list: it isn't packaged for
# Mint 22.3 / Ubuntu 24.04. The README documents the manual build.

DEBIAN_PKGS=(
    # Build toolchain
    build-essential
    pkg-config
    libssl-dev
    # PipeWire / ALSA
    libpipewire-0.3-dev
    libasound2-dev
    # Tauri webview + tray
    libwebkit2gtk-4.1-dev
    libayatana-appindicator3-dev
    librsvg2-dev
    libxdo-dev
    # Mic effect chain (LV2/LADSPA plugins)
    lsp-plugins-lv2
    lilv-utils
)

FEDORA_PKGS=(
    gcc
    gcc-c++
    pkgconf-pkg-config
    openssl-devel
    pipewire-devel
    alsa-lib-devel
    webkit2gtk4.1-devel
    libappindicator-gtk3-devel
    librsvg2-devel
    libxdo-devel
    lsp-plugins-lv2
    lilv-tools
)

ARCH_PKGS=(
    base-devel
    pkgconf
    openssl
    pipewire
    alsa-lib
    webkit2gtk-4.1
    libappindicator-gtk3
    librsvg
    xdotool
    lsp-plugins
    lilv
)

# --- Helpers -----------------------------------------------------------

detect_os_id() {
    if [[ -r /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        echo "${ID_LIKE:-$ID}" | tr '[:upper:]' '[:lower:]' | awk '{print $1}'
    else
        echo "unknown"
    fi
}

# Print the right install command for this distro family without
# running it; useful for --check.
suggest_install_cmd() {
    local id="$1"
    case "$id" in
        debian|ubuntu) echo "sudo apt install -y ${DEBIAN_PKGS[*]}" ;;
        fedora|rhel)   echo "sudo dnf install -y ${FEDORA_PKGS[*]}" ;;
        arch|manjaro)  echo "sudo pacman -S --needed ${ARCH_PKGS[*]}" ;;
        *) echo "(unsupported distro family '$id'; install pipewire/webkit2gtk/lsp-plugins manually)" ;;
    esac
}

run_install_cmd() {
    local id; id="$(detect_os_id)"
    case "$id" in
        debian|ubuntu)
            echo "Installing Debian/Ubuntu packages (sudo will prompt for password)…"
            sudo apt install -y "${DEBIAN_PKGS[@]}"
            ;;
        fedora|rhel)
            echo "Installing Fedora/RHEL packages (sudo will prompt for password)…"
            sudo dnf install -y "${FEDORA_PKGS[@]}"
            ;;
        arch|manjaro)
            echo "Installing Arch packages (sudo will prompt for password)…"
            sudo pacman -S --needed "${ARCH_PKGS[@]}"
            ;;
        *)
            echo "Unsupported distro family '$id'." >&2
            echo "Install manually: $(suggest_install_cmd "$id")" >&2
            return 1
            ;;
    esac
}

check_runtime() {
    local required_missing=() optional_missing=()
    pkg-config --exists libpipewire-0.3 2>/dev/null || required_missing+=("pipewire-dev")
    pkg-config --exists webkit2gtk-4.1 2>/dev/null   || required_missing+=("webkit2gtk-4.1-dev")
    {
        [[ -d /usr/lib/lv2/lsp-plugins.lv2 ]] \
            || find /usr/lib /usr/lib64 -maxdepth 3 -name 'lsp-*.lv2' 2>/dev/null | grep -q .
    } || required_missing+=("lsp-plugins-lv2")
    command -v cargo >/dev/null 2>&1 || required_missing+=("rust (rustup.rs)")
    command -v node  >/dev/null 2>&1 || [[ -x "$HOME/.local/node/bin/node" ]] \
        || required_missing+=("node 20.19+ (https://nodejs.org/dist/)")
    command -v cargo-tauri >/dev/null 2>&1 || [[ -x "$HOME/.cargo/bin/cargo-tauri" ]] \
        || required_missing+=("cargo-tauri (cargo install tauri-cli --version ^2.0)")
    [[ -e /usr/lib/ladspa/librnnoise_ladspa.so ]] \
        || optional_missing+=("librnnoise_ladspa.so — noise suppression slot stays inert")

    if [[ ${#required_missing[@]} -eq 0 && ${#optional_missing[@]} -eq 0 ]]; then
        echo "All dependencies present (including optional)."
        return 0
    fi
    if [[ ${#required_missing[@]} -gt 0 ]]; then
        echo "Required dependencies missing:"
        printf '  - %s\n' "${required_missing[@]}"
    else
        echo "All required dependencies present."
    fi
    if [[ ${#optional_missing[@]} -gt 0 ]]; then
        echo
        echo "Optional (Undertone runs without these):"
        printf '  - %s\n' "${optional_missing[@]}"
    fi
    if [[ ${#required_missing[@]} -gt 0 ]]; then
        echo
        echo "Distro install line:"
        echo "  $(suggest_install_cmd "$(detect_os_id)")"
        return 1
    fi
    return 0
}

# --- Modes -------------------------------------------------------------

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
        echo "  undertone                                       # launch app (auto-starts daemon)"
        echo "  systemctl --user enable --now undertone-daemon  # auto-start daemon at login"
        echo "  ./packaging/install.sh --deps                   # if you haven't installed system deps yet"
        ;;
    --enable|enable)
        systemctl --user daemon-reload 2>/dev/null || true
        systemctl --user enable --now undertone-daemon.service
        echo "Daemon enabled and started."
        systemctl --user status undertone-daemon.service --no-pager | head -5
        ;;
    --deps|deps)
        run_install_cmd
        ;;
    --check|check)
        check_runtime
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
    -h|--help|help)
        cat <<EOF
Usage: $0 [mode]

Modes:
  install (default)   Copy launcher / desktop entry / systemd unit into
                      ~/.local/ and ~/.config/.
  --enable            Install AND enable+start the systemd user unit.
  --deps              Detect OS family and install the required system
                      packages via apt / dnf / pacman (sudo will prompt).
  --check             Report which dependencies look present vs missing.
                      Doesn't change anything; safe to run anytime.
  --uninstall         Remove the user-level files. Doesn't touch deps.

Environment:
  UNDERTONE_REPO      Override the source-tree root (default \$HOME/Undertone).
  XDG_BIN_HOME        Override the launcher install dir (default ~/.local/bin).
EOF
        ;;
    *)
        echo "Usage: $0 [install|--enable|--deps|--check|--uninstall|--help]" >&2
        exit 1
        ;;
esac
