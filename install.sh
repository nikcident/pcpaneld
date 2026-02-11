#!/usr/bin/env bash
set -euo pipefail

REPO="nikcident/pcpaneld"
BINARY_NAME="pcpaneld"
INSTALL_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.config/systemd/user"
UDEV_RULE_PATH="/etc/udev/rules.d/70-pcpanel.rules"

# --- Colors ---

if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED='' GREEN='' YELLOW='' BLUE='' BOLD='' RESET=''
fi

info()  { printf "${BLUE}::${RESET} %s\n" "$*"; }
ok()    { printf "${GREEN}::${RESET} %s\n" "$*"; }
warn()  { printf "${YELLOW}:: %s${RESET}\n" "$*"; }
err()   { printf "${RED}:: %s${RESET}\n" "$*" >&2; }

# --- Checks ---

ARCH="$(uname -m)"
if [ "$ARCH" != "x86_64" ]; then
    err "Unsupported architecture: $ARCH (only x86_64 binaries are available)"
    err "Build from source instead: https://github.com/$REPO#building"
    exit 1
fi

if ! command -v curl &>/dev/null; then
    err "curl is required but not found"
    exit 1
fi

# --- Detect latest version ---

info "Fetching latest release..."
RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")"
VERSION="$(printf '%s' "$RELEASE_JSON" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"v([^"]+)".*/\1/')"

if [ -z "$VERSION" ]; then
    err "Failed to detect latest release version"
    exit 1
fi

ok "Latest version: v$VERSION"

# --- Download binary ---

ASSET_NAME="pcpaneld-x86_64-unknown-linux-gnu"
DOWNLOAD_URL="https://github.com/$REPO/releases/download/v$VERSION/$ASSET_NAME"

TMPFILE="$(mktemp)"
trap 'rm -f "$TMPFILE"' EXIT

info "Downloading $ASSET_NAME..."
curl -fSL --progress-bar -o "$TMPFILE" "$DOWNLOAD_URL"

# --- Stop running daemon if upgrading ---

if systemctl --user is-active "$BINARY_NAME" &>/dev/null; then
    warn "Stopping running pcpaneld service for upgrade..."
    systemctl --user stop "$BINARY_NAME"
fi

# --- Install binary ---

info "Installing to $INSTALL_DIR/$BINARY_NAME..."
install -Dm755 "$TMPFILE" "$INSTALL_DIR/$BINARY_NAME"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    warn "$INSTALL_DIR is not in your PATH"
    warn "Add it to your shell profile:  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

# --- Install systemd service ---

info "Installing systemd user service..."
mkdir -p "$SERVICE_DIR"
cat > "$SERVICE_DIR/$BINARY_NAME.service" << 'UNIT'
[Unit]
Description=PCPanel Pro Daemon
After=pipewire-pulse.service graphical-session.target
Wants=pipewire-pulse.service

[Service]
ExecStart=%h/.local/bin/pcpaneld daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
UNIT

systemctl --user daemon-reload

# --- Install udev rule ---

UDEV_RULE='KERNEL=="hidraw*", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="a3c5", MODE="0660", TAG+="uaccess"'

if [ -f "$UDEV_RULE_PATH" ]; then
    ok "udev rule already installed at $UDEV_RULE_PATH"
else
    printf "\n"
    printf "%bThe PCPanel needs a udev rule for device access.%b\n" "$BOLD" "$RESET"
    printf "This will run: sudo install -Dm644 ... %s\n" "$UDEV_RULE_PATH"
    printf "\n"
    read -rp "Install udev rule now? [y/N] " answer
    case "$answer" in
        [yY]|[yY][eE][sS])
            echo "$UDEV_RULE" | sudo install -Dm644 /dev/stdin "$UDEV_RULE_PATH"
            sudo udevadm control --reload-rules && sudo udevadm trigger
            ok "udev rule installed"
            ;;
        *)
            warn "Skipped udev rule. Install it manually:"
            warn "  echo '$UDEV_RULE' | sudo tee $UDEV_RULE_PATH"
            warn "  sudo udevadm control --reload-rules && sudo udevadm trigger"
            ;;
    esac
fi

# --- Enable and start ---

info "Enabling and starting pcpaneld service..."
systemctl --user enable --now "$BINARY_NAME"

printf "\n"
ok "${BOLD}pcpaneld v$VERSION installed successfully!${RESET}"
ok "Run '${BOLD}pcpaneld info${RESET}' to verify the daemon is running."
