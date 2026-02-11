# pcpaneld

[![CI](https://github.com/nikcident/pcpaneld/actions/workflows/ci.yml/badge.svg)](https://github.com/nikcident/pcpaneld/actions/workflows/ci.yml)

Native Linux daemon for the [PCPanel Pro](https://www.getpcpanel.com/)
USB audio mixer. Map knobs and sliders to system volume, per-app volume,
or focused-window audio. Buttons can mute, control media playback, or
run shell commands.

I bought a PCPanel Pro and switched to Linux as my daily driver. The
official software is Windows-only, and the community version is Java --
so I wrote a native daemon in Rust.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/nikcident/pcpaneld/main/install.sh | bash
```

To install a specific version (including pre-releases):

```bash
curl -fsSL https://raw.githubusercontent.com/nikcident/pcpaneld/main/install.sh | bash -s -- v0.3.0-beta
```

This downloads a pre-built binary to `~/.local/bin/`, installs a systemd
user service, and optionally sets up the udev rule for device access.

> `~/.local/bin` must be in your `PATH` (it is by default on most distros).

To build from source instead, see [Building](#building) below.

## Uninstall

```bash
systemctl --user disable --now pcpaneld
rm ~/.local/bin/pcpaneld
rm ~/.config/systemd/user/pcpaneld.service
systemctl --user daemon-reload
sudo rm /etc/udev/rules.d/70-pcpanel.rules
```

Your configuration in `~/.config/pcpaneld/` is intentionally preserved.

## Features

- **Per-app volume** -- bind a slider to Spotify, a knob to Discord, etc.
- **Focused-app mode** -- automatically controls whichever window has focus
  (KDE Plasma on Wayland only for now)
- **Mute toggle** on knob press
- **Media controls** -- play/pause, next/prev via MPRIS D-Bus
- **Shell commands** -- trigger any command on button press
- **Auto-reconnect** -- unplug and replug without restarting the daemon
- **Live config reload** -- edit the config, changes apply instantly via inotify
- **Signal processing** -- per-control jitter suppression, debounce, configurable volume curve
- **System tray** -- StatusNotifierItem integration

## Requirements

- Linux with PulseAudio or PipeWire (PulseAudio compat layer -- the
  default on most modern distros)
- **Optional:** KDE Plasma on Wayland for focused-app tracking. Everything
  else works on any desktop environment.

## Building

You need a Rust toolchain (via [rustup](https://rustup.rs/)) and a few
C libraries for linking.

### Build dependencies

**Fedora:**
```
sudo dnf install hidapi-devel pulseaudio-libs-devel dbus-devel systemd-devel gcc pkg-config
```

**Debian / Ubuntu:**
```
sudo apt install libhidapi-dev libpulse-dev libdbus-1-dev libsystemd-dev libudev-dev gcc pkg-config
```

**Arch:**
```
sudo pacman -S hidapi libpulse dbus systemd-libs gcc pkgconf
```

> **Note:** Debian and Arch package names have not been verified. If you
> hit issues, please open an issue or PR with corrections.

### Immutable distros (Bazzite, Silverblue, Kinoite)

On immutable Fedora variants where `/usr` is read-only, use a distrobox:

```
just setup
export PCPANELD_CARGO="distrobox enter pcpaneld-dev --"
```

Add the export to your shell profile so it persists. I developed this
project on Bazzite, so the distrobox workflow is well-tested.

### Build and install

```
just build           # compile
just install         # install binaries, udev rule, and systemd service
```

Then enable and start the service:

```
systemctl --user enable --now pcpaneld
```

## Quick start

```bash
# Check that the daemon sees the device
pcpaneld info

# See what audio apps are running (find binary names for app matching)
pcpaneld apps

# Assign slider 1 to control Spotify's volume
pcpaneld assign slider1 volume app --binary spotify

# Assign knob 1 to system volume with mute on press (this is the default)
pcpaneld assign knob1 volume default-output
pcpaneld assign knob1 mute default-output

# Assign slider 4 to control the focused window's audio
pcpaneld assign slider4 volume focused

# Assign knob 3 button to play/pause media
pcpaneld assign knob3 media play_pause

# Run a shell command on knob 4 press
pcpaneld assign knob4 exec "notify-send 'Button pressed!'"

# Remove an assignment
pcpaneld unassign slider3

# View current config
pcpaneld config show
```

## CLI reference

All commands communicate with the running daemon over a Unix socket.

| Command | Description |
|---------|-------------|
| `pcpaneld --version` | Print version |
| `pcpaneld info` | Show device connection, PulseAudio status, and control mappings |
| `pcpaneld apps` | List running audio applications with their binary names and Flatpak IDs |
| `pcpaneld devices` | List audio devices (outputs and inputs) |
| `pcpaneld assign <control> <action> <value> [--binary B] [--name N] [--flatpak-id ID]` | Assign an action to a control |
| `pcpaneld unassign <control>` | Remove a control assignment |
| `pcpaneld config show` | Print the current config as TOML |
| `pcpaneld config reload` | Force the daemon to reload the config file |
| `pcpaneld config dir` | Print the config directory path |

### Assign parameters

**Controls:** `knob1`-`knob5`, `slider1`-`slider4`

**Actions:** `volume` (analog dial/slider), `mute` (knob button), `media` (knob button), `exec` (knob button)

**Values** (third positional arg, meaning depends on action):
- For `volume`/`mute`: an audio target (`default-output`, `default-input`, `app`, `focused`)
- For `media`: a media command (`play_pause`, `play`, `pause`, `next`, `previous`, `stop`)
- For `exec`: a shell command string

**Audio targets** (for `volume`/`mute`):
- `default-output` -- system audio output (or `default-sink` for backwards compatibility)
- `default-input` -- system audio input/microphone (or `default-source` for backwards compatibility)
- `app` -- a specific application (requires at least one of `--binary`, `--name`, `--flatpak-id`)
- `focused` -- whichever application has window focus (KDE Plasma)

### Examples

```bash
# Control Discord volume with knob 3, mute on press
pcpaneld assign knob3 volume app --binary Discord
pcpaneld assign knob3 mute app --binary Discord

# Control a Flatpak app by its Flatpak ID
pcpaneld assign slider2 volume app --flatpak-id com.valvesoftware.Steam

# Match by both binary and name (AND logic -- both must match)
pcpaneld assign slider3 volume app --binary firefox --name Firefox

# Microphone volume on knob 2
pcpaneld assign knob2 volume default-input
pcpaneld assign knob2 mute default-input

# Media controls on knob 4: play/pause on press
pcpaneld assign knob4 media play_pause

# Skip to next track on knob 5 press
pcpaneld assign knob5 media next

# Run a custom script on knob 5 press
pcpaneld assign knob5 exec "~/.local/bin/my-script.sh"
```

## Configuration

The config file lives at `~/.config/pcpaneld/config.toml`. It's created automatically on first run with sensible defaults.

Changes are detected via inotify and applied immediately -- no daemon restart needed. You can edit the file by hand or use `pcpaneld assign`/`unassign` which modify it for you.

LED behavior (which zones are lit) is also configurable -- see `[leds]` in the config reference.

See [docs/configuration.md](docs/configuration.md) for the full config reference.

## Troubleshooting

### Device not detected

1. Check that the udev rule is installed:
   ```bash
   cat /etc/udev/rules.d/70-pcpanel.rules
   ```
2. Check that the device shows up:
   ```bash
   ls /dev/hidraw*
   ```
3. Reload udev rules after installing:
   ```bash
   sudo udevadm control --reload-rules && sudo udevadm trigger
   ```
4. Unplug and replug the device.

### Permission denied

On SELinux-enforcing systems, check for AVC denials:

```bash
sudo ausearch -m avc -ts recent | grep hidraw
```

The `TAG+="uaccess"` rule should grant access to the logged-in user via systemd-logind. If denials appear, check that `logind` is managing your session (`loginctl session-status`).

### Daemon won't start

Check the service logs:

```bash
journalctl --user -u pcpaneld -n 50
```

Common issues:
- PipeWire/PulseAudio not running: the daemon retries with backoff, but check `systemctl --user status pipewire-pulse`
- Stale socket file: the daemon auto-cleans stale sockets on startup, but if it fails, remove `$XDG_RUNTIME_DIR/pcpaneld.sock` manually

### Focused app not working

Focused-app tracking requires KDE Plasma on Wayland. The daemon loads a KWin script via D-Bus. If it fails, check:

```bash
journalctl --user -u pcpaneld | grep -i kwin
```

The feature degrades gracefully -- if KWin is unavailable, `focused` targets simply do nothing. All other controls work normally.

### Wrong app matched

Use `pcpaneld apps` to see the binary name, application name, and Flatpak ID of every running audio stream. Use those exact values in your `--binary`, `--name`, or `--flatpak-id` flags. Matching is case-insensitive substring.

## Architecture

See [docs/architecture.md](docs/architecture.md) for internals documentation.

## Acknowledgments

The HID protocol was figured out by studying
[nvdweem/PCPanel](https://github.com/nvdweem/PCPanel), the community
Java controller for PCPanel devices. No code was copied -- the Rust
implementation is original -- but that project was an essential reference
for understanding the device protocol.

## Built with AI

Built with [Claude](https://claude.ai/) (Anthropic). I designed the
architecture and directed the implementation -- Claude wrote most of the
code. AI was the primary development tool for this project.

## License

[MIT](LICENSE)
