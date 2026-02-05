# Configuration Reference

## File location

The config file is at `~/.config/pcpaneld/config.toml` (respects `$XDG_CONFIG_HOME`).

On first run, the daemon creates this file with default contents if it doesn't exist. You can also create it manually or use `pcpaneld assign` to build it incrementally.

Find the exact path on your system:

```bash
pcpaneld config dir
```

## Live reload

The daemon watches the config directory with inotify. Any write to `config.toml` is detected and applied automatically -- no restart needed.

There is a 50ms debounce to handle editors that write files in multiple steps (write temp file, rename). Saves made by the daemon itself (via `pcpaneld assign`) are suppressed to avoid redundant reloads.

You can also force a reload explicitly:

```bash
pcpaneld config reload
```

## Full annotated example

```toml
# PCPanel Pro configuration
# Managed by pcpaneld, but human-editable.
# Changes are auto-detected via inotify -- no manual reload needed.

[device]
# serial = "YOUR_SERIAL_HERE"  # optional: lock to specific device

[signal]
slider_rolling_average = 5
slider_delta_threshold = 2
slider_debounce_ms = 10
knob_rolling_average = 3
knob_delta_threshold = 1
knob_debounce_ms = 0
volume_exponent = 1.0  # 1.0=linear (PA handles perception), >1.0 adds quiet-end resolution

# System volume + mute on knob 1
[controls.knob1]
dial = { type = "volume", target = { type = "default_output" } }
button = { type = "mute", target = { type = "default_output" } }

# Microphone volume + mute on knob 2
[controls.knob2]
dial = { type = "volume", target = { type = "default_input" } }
button = { type = "mute", target = { type = "default_input" } }

# Discord on knob 3
[controls.knob3]
dial = { type = "volume", target = { type = "app", matcher = { binary = "Discord" } } }
button = { type = "mute", target = { type = "app", matcher = { binary = "Discord" } } }

# Spotify on slider 1
[controls.slider1]
dial = { type = "volume", target = { type = "app", matcher = { binary = "spotify" } } }

# Firefox (Flatpak) on slider 2
[controls.slider2]
dial = { type = "volume", target = { type = "app", matcher = { flatpak_id = "org.mozilla.firefox" } } }

# Play/pause media on knob 4 press
[controls.knob4]
dial = { type = "volume", target = { type = "default_output" } }
button = { type = "media", command = "play_pause" }

# Run a script on knob 5 press
[controls.knob5]
button = { type = "exec", command = "notify-send 'PCPanel button pressed!'" }

# Focused window on slider 4 (KDE Plasma only)
[controls.slider4]
dial = { type = "volume", target = { type = "focused_app" } }

[leds]
knobs = true          # LED rings on rotary knobs
sliders = true        # LED strips on sliders
slider_labels = true  # LED labels above sliders
logo = true           # Logo LED
```

## Sections

### `[device]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `serial` | string (optional) | none | Lock the daemon to a specific device by USB serial number. Omit to use any connected PCPanel Pro. Reserved for future multi-device support. |

### `[signal]`

Controls the signal processing pipeline that filters hardware noise before changing volume. Each physical control (knob/slider) gets its own pipeline instance.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `slider_rolling_average` | integer | `5` | Rolling average window size for sliders. Larger = smoother but laggier. Minimum 1. |
| `slider_delta_threshold` | integer | `2` | Minimum change from last emitted value to emit a new one (sliders). Suppresses jitter when the slider is stationary. |
| `slider_debounce_ms` | integer | `10` | Minimum milliseconds between emissions (sliders). Prevents flooding. |
| `knob_rolling_average` | integer | `3` | Rolling average window size for knobs. |
| `knob_delta_threshold` | integer | `1` | Minimum change from last emitted value (knobs). |
| `knob_debounce_ms` | integer | `0` | Minimum milliseconds between emissions (knobs). Default 0 because knobs are turned deliberately, not subject to the same resting jitter as sliders. |
| `volume_exponent` | float | `1.0` | Power curve exponent for mapping hardware position to volume. See below. |

#### Volume exponent explained

The raw hardware value (0-255) is mapped to a linear volume factor (0.0-1.0) using a power curve:

```
volume = (hw_value / 255) ^ exponent
```

This factor is then passed to PulseAudio using its linear-to-perceptual conversion (`pa_sw_volume_from_linear`). PulseAudio's volume scale already applies perceptual (cubic) weighting internally, so with the default exponent of 1.0, slider position maps directly to perceived volume percentage — 50% travel ≈ 50% perceived loudness.

| Exponent | Effect | Midpoint | When to use |
|----------|--------|----------|-------------|
| `1.0` | Linear (default) | 50% | Slider position = volume percentage. Works for most users. |
| `2.0` | Extra quiet-end resolution | ~25% | More fine control at low volumes. |
| `3.0` | Strong quiet-end bias | ~12.5% | You mostly work at very low volumes. |

The curve always maps 0 to silence and 255 to full volume regardless of exponent.

#### Signal pipeline stages

The pipeline processes each hardware reading through these stages in order:

1. **Endpoint bypass**: Values 0 and 255 always pass through immediately, bypassing all other stages. This ensures you can always reach silence and full volume.

2. **Rolling average**: Maintains a sliding window of recent readings and outputs their average. Smooths out electrical noise and ADC jitter. Larger window = smoother but adds latency.

3. **Delta threshold**: Suppresses the output if the change from the last emitted value is less than the threshold. Prevents micro-adjustments when the control is at rest but the ADC reads slightly different values.

4. **Debounce**: Suppresses the output if less than N milliseconds have elapsed since the last emission. Rate-limits volume changes to prevent flooding PulseAudio.

#### Tuning guidance

The defaults work well for most users. If you experience issues:

- **Slider jitters when not being touched**: increase `slider_delta_threshold` (try 3-5) or `slider_rolling_average` (try 7-10)
- **Knob feels laggy**: decrease `knob_rolling_average` to 1 (disables smoothing)
- **Volume jumps from 0 to loud too quickly**: increase `volume_exponent` (try 2.0)
- **Want more fine control at low volumes**: increase `volume_exponent` (try 2.0 or 3.0)
- **Volume changes feel choppy**: decrease `slider_delta_threshold` to 1

### `[controls.*]`

Each control is configured in a `[controls.NAME]` section where NAME is one of:

- `knob1` through `knob5` -- rotary encoders with push buttons
- `slider1` through `slider4` -- linear sliders (no buttons)

Each control has two optional fields:

| Field | Type | Applies to | Description |
|-------|------|------------|-------------|
| `dial` | action | knobs and sliders | What happens when the control is turned/moved |
| `button` | action | knobs only | What happens when the knob is pressed |

If a control has no section in the config, it does nothing.

#### Dial actions

```toml
dial = { type = "volume", target = { ... } }
```

`volume` is currently the only dial action type. It maps the control's physical position to the target's volume through the signal pipeline and volume curve.

#### Button actions

##### `mute` -- toggle mute

```toml
button = { type = "mute", target = { ... } }
```

Each press toggles mute on the specified audio target. See [Audio targets](#audio-targets) for valid target types.

##### `media` -- MPRIS media control

```toml
button = { type = "media", command = "play_pause" }
```

Sends a media command to the most appropriate MPRIS media player via D-Bus (prefers the player that is currently playing).

Valid commands: `play_pause`, `play`, `pause`, `next`, `previous`, `stop`.

##### `exec` -- shell command

```toml
button = { type = "exec", command = "notify-send 'Button pressed!'" }
```

Runs the command via `sh -c`. Fire-and-forget; non-zero exit is logged as a warning.

### `[leds]`

Controls which LED zones on the device are enabled. Disabled zones are sent an all-off command. Changes take effect on config reload and device reconnect.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `knobs` | bool | `true` | Enable LED ring on rotary knobs |
| `sliders` | bool | `true` | Enable LED strip on sliders |
| `slider_labels` | bool | `true` | Enable LED labels above sliders |
| `logo` | bool | `true` | Enable the logo LED |

Example -- disable everything except knob rings:

```toml
[leds]
knobs = true
sliders = false
slider_labels = false
logo = false
```

### Audio targets

Every action requires a `target` that specifies what audio stream to control.

#### `default_output` -- system output

```toml
target = { type = "default_output" }
```

Controls the default audio output device (speakers/headphones). Follows PulseAudio's default sink -- if you switch outputs, the control follows.

Note: `default_sink` is also accepted for backwards compatibility.

#### `default_input` -- system input

```toml
target = { type = "default_input" }
```

Controls the default audio input device (microphone).

Note: `default_source` is also accepted for backwards compatibility.

#### `app` -- specific application

```toml
target = { type = "app", matcher = { binary = "spotify" } }
target = { type = "app", matcher = { name = "Firefox" } }
target = { type = "app", matcher = { flatpak_id = "org.mozilla.firefox" } }
target = { type = "app", matcher = { binary = "Discord", name = "Discord" } }
```

Controls a specific application's audio stream (PulseAudio sink-input).

Matcher fields:

| Field | Matches PA property | Description |
|-------|---------------------|-------------|
| `binary` | `application.process.binary` | The process binary name |
| `name` | `application.name` | The application's self-reported name |
| `flatpak_id` | `application.flatpak.id` | The Flatpak application ID (for sandboxed apps where `binary` might be `bwrap`) |

**Matching rules:**
- Each field is a **case-insensitive substring** match
- When multiple fields are set, **all must match** (AND logic)
- An empty matcher (no fields) matches nothing
- If multiple streams match, the volume is applied to all of them

**Finding the right values:** Use `pcpaneld apps` to see every running audio stream with its binary, name, and Flatpak ID. Use those values in your config.

```bash
$ pcpaneld apps
# Look for the binary, name, and flatpak_id fields in the output
```

#### `focused_app` -- currently focused window

```toml
target = { type = "focused_app" }
```

Controls whichever application currently has window focus. The daemon tracks the focused window via a KWin script loaded over D-Bus.

**Requirements:** KDE Plasma on Wayland. On other desktop environments, this target silently does nothing -- all other controls continue to work.

**How matching works:** When the focused window changes, the daemon matches the window's properties against running audio streams using (in priority order):

1. Window's `desktopFile` against the stream's `flatpak_id`
2. Window's `resourceName` against the stream's `binary`
3. Window's `desktopFile` against the stream's `binary`
4. Window's `resourceClass` against the stream's `binary`

When a control's dial action targets `focused_app`, the daemon re-applies the last volume set by that control to the newly focused app's streams.
