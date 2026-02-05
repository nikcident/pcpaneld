# Override to build via distrobox: export PCPANELD_CARGO="distrobox enter pcpaneld-dev --"
cargo_prefix := env("PCPANELD_CARGO", "")

default: check

# Lint and format check (no C deps needed, works on host)
check:
    cargo clippy --workspace -- -D warnings
    cargo fmt --check --all

# Apply rustfmt
fmt:
    cargo fmt --all

build:
    {{cargo_prefix}} cargo build --workspace

test:
    {{cargo_prefix}} cargo test --workspace

release:
    {{cargo_prefix}} cargo build --workspace --release

all: check build test

install: install-binaries install-service install-udev

install-binaries: release
    install -Dm755 target/release/pcpaneld ~/.cargo/bin/pcpaneld

install-udev:
    sudo cp dist/70-pcpanel.rules /etc/udev/rules.d/
    sudo udevadm control --reload-rules
    sudo udevadm trigger

install-service:
    install -Dm644 dist/pcpaneld.service ~/.config/systemd/user/pcpaneld.service
    systemctl --user daemon-reload

clean:
    cargo clean

# Run CI workflow locally via act (uses podman)
act *args='push':
    act --container-daemon-socket $XDG_RUNTIME_DIR/podman/podman.sock {{args}}

# Run each CI job separately, logs to .act-logs/
act-each:
    #!/usr/bin/env bash
    set -uo pipefail
    mkdir -p .act-logs
    failed=0
    for job in check build-debian build-arch; do
        printf "  %-20s" "$job"
        if act push --container-daemon-socket "$XDG_RUNTIME_DIR/podman/podman.sock" \
            -j "$job" > ".act-logs/${job}.log" 2>&1; then
            echo "ok"
        else
            echo "FAILED  ->  .act-logs/${job}.log"
            failed=1
        fi
    done
    exit $failed

# Create a distrobox with build deps (for immutable distros)
setup:
    distrobox create --name pcpaneld-dev --image registry.fedoraproject.org/fedora:43
    distrobox enter pcpaneld-dev -- sudo dnf install -y \
        dbus-devel systemd-devel pulseaudio-libs-devel hidapi-devel gcc pkg-config
