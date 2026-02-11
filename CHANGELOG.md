# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.0] - 2026-02-11

### Added

- One-liner install script (`curl | bash`) with systemd service and udev rule setup
- GitHub Release workflow: builds stripped binary on tag push, validates version against Cargo.toml
- Install test harness (`tests/test-install.sh`) for verifying installer in disposable containers
- `just deploy` and `just deploy-debug` recipes for quick iteration

### Fixed

- KWin script loading now retries up to 5 times with linear backoff at login, fixing race condition where KWin wasn't ready yet
- systemd service orders after `graphical-session.target` to improve boot reliability

## [0.2.0] - 2026-02-08

### Fixed

- Focused-app slider now works with Wine/Proton games that spawn separate processes for the window and audio stream. Previously only exact PID matching was used (Strategy 6), which failed when the KWin window PID and PulseAudio stream PID differed. Now matches via process group (PGID) and sibling detection (same parent PID) as fallbacks.

## [0.1.1] - 2026-02-05

### Added

- Version banner and active config dump logged at startup (INFO level)
- Config dump logged on reload (DEBUG level) for both file-watcher and IPC reloads
- `pcpaneld --version` flag
- justfile targets: `start`, `stop`, `restart`, `enable`, `disable`, `status`, `logs` (systemd management), `deny` (cargo-deny)
- CLAUDE.md workflow section with branch/PR conventions

### Changed

- `install-udev` skips sudo when the rule file is already up to date

## [0.1.0] - 2026-02-05

Initial release.

### Added

- Per-app volume control (match by binary name, application name, or Flatpak ID)
- Focused-app mode: automatically controls whichever window has focus (KDE Plasma Wayland)
- Default output and default input (microphone) volume and mute
- Mute toggle on knob press
- Media controls (play/pause, next, previous, stop) via MPRIS D-Bus
- Shell command execution on button press
- Signal processing pipeline: per-control jitter suppression, debounce, configurable volume curve
- Auto-reconnect on USB unplug/replug via udev monitoring
- Live config reload via inotify (no daemon restart needed)
- CLI tool for assigning controls, listing apps/devices, and managing config
- IPC over Unix socket between CLI and daemon
- System tray integration (StatusNotifierItem via ksni)
- LED control for knobs, sliders, slider labels, and logo
- systemd user service for daemon management
- udev rule for hidraw device access
- Multi-distro CI (Fedora, Debian, Arch)
