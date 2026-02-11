#!/usr/bin/env bash
# Verifies install.sh in a disposable container.
#
# Usage (from repo root):
#   podman run --rm -v ./install.sh:/install.sh:ro,z \
#     -v ./tests/test-install.sh:/test-install.sh:ro,z \
#     fedora:43 bash /test-install.sh [VERSION]
#
# VERSION defaults to the latest GitHub release. Pass a tag like
# "v0.2.0-test" to test a specific (including pre-release) version.

set -euo pipefail

VERSION="${1:-}"

# Install test dependency ('file' command)
dnf install -y file >/dev/null 2>&1

# --- Stubs for commands unavailable in a minimal container ---

mkdir -p /usr/local/bin

cat > /usr/local/bin/systemctl << 'STUB'
#!/bin/bash
echo "[systemctl] $*"
if [[ "$*" == *"is-active"* ]]; then exit 1; fi
exit 0
STUB
chmod +x /usr/local/bin/systemctl

cat > /usr/local/bin/sudo << 'STUB'
#!/bin/bash
echo "[sudo] $*"
"$@"
STUB
chmod +x /usr/local/bin/sudo

cat > /usr/local/bin/udevadm << 'STUB'
#!/bin/bash
echo "[udevadm] $*"
STUB
chmod +x /usr/local/bin/udevadm

# --- Run installer (answer "y" to udev prompt) ---

if [ -n "$VERSION" ]; then
    echo y | bash /install.sh "$VERSION"
else
    echo y | bash /install.sh
fi

# --- Verification ---

echo ""
echo "========================================="
echo "           VERIFICATION"
echo "========================================="

PASS=0
FAIL=0

check() {
    if eval "$2"; then
        echo "PASS: $1"
        PASS=$((PASS + 1))
    else
        echo "FAIL: $1"
        FAIL=$((FAIL + 1))
    fi
}

echo ""
echo "--- Binary ---"
check "binary exists" "[ -f ~/.local/bin/pcpaneld ]"
check "binary is executable" "[ -x ~/.local/bin/pcpaneld ]"
check "binary is ELF" "file ~/.local/bin/pcpaneld | grep -q ELF"
check "binary permissions are 755" "stat -c %a ~/.local/bin/pcpaneld | grep -q 755"

echo ""
echo "--- Service file ---"
check "service file exists" "[ -f ~/.config/systemd/user/pcpaneld.service ]"
check "ExecStart points to ~/.local/bin" "grep -q 'ExecStart=%h/.local/bin/pcpaneld daemon' ~/.config/systemd/user/pcpaneld.service"
check "has Restart=on-failure" "grep -q 'Restart=on-failure' ~/.config/systemd/user/pcpaneld.service"
check "has WantedBy=default.target" "grep -q 'WantedBy=default.target' ~/.config/systemd/user/pcpaneld.service"
check "orders after pipewire-pulse" "grep -q 'After=pipewire-pulse.service' ~/.config/systemd/user/pcpaneld.service"
check "orders after graphical-session" "grep -q 'graphical-session.target' ~/.config/systemd/user/pcpaneld.service"

echo ""
echo "--- udev rule ---"
check "udev rule exists" "[ -f /etc/udev/rules.d/70-pcpanel.rules ]"
check "rule has correct vendor ID" "grep -q '0483' /etc/udev/rules.d/70-pcpanel.rules"
check "rule has correct product ID" "grep -q 'a3c5' /etc/udev/rules.d/70-pcpanel.rules"
check "rule has uaccess tag" "grep -q 'uaccess' /etc/udev/rules.d/70-pcpanel.rules"

echo ""
echo "========================================="
echo "Results: $PASS passed, $FAIL failed"
echo "========================================="
[ "$FAIL" -eq 0 ]
