# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
