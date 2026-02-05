# Architecture

Internals reference for contributors. Read the [README](../README.md) first for what the project does.

## Crate layout

```
pcpaneld/
  crates/
    pcpaneld-core/    # Shared types, config, IPC wire format, HID parsing
    pcpaneld/         # The binary (daemon + CLI)
```

**`pcpaneld-core`** is a library crate with zero system dependencies at runtime. It defines all shared types: config structs, audio types, control IDs, HID report parsing, IPC request/response types, and the wire format. Error types use `thiserror`.

**`pcpaneld`** is the main binary with subcommands for daemon and CLI operations. When run with `pcpaneld daemon`, it starts the daemon which owns all mutable state. HID and PulseAudio run on `std::thread` (their C libraries require blocking APIs). Everything else is `tokio` tasks. The central engine is a `tokio::select!` loop that receives from all subsystems. Error handling uses `anyhow`. When run with other subcommands (e.g., `pcpaneld info`, `pcpaneld apps`), it acts as a CLI client connecting to the daemon via Unix socket.

## Thread and task model

```
std::thread                           tokio tasks
-----------                           -----------

  +-----------+                       +-------------+
  | udev      |---DeviceEvent------->| HID thread  |
  | monitor   |  sync_channel(4)     | (see below) |
  +-----------+                       +---+---------+
                                          |
                          watch([u8;9])   |  mpsc(32)
                      (positions,latest)  |  (buttons)
                                          v
                                    +===========+
+-----------+    mpsc(32)           ||         ||    mpsc(8)     +--------+
| PulseAudio|---AudioNotification-->|| ENGINE  ||<--IpcMessage---| IPC    |
| thread    |<--AudioCommand--------||         ||---IpcResponse->| server |
+-----------+    mpsc(32)           ||         ||    oneshot      +--------+
                                    +===========+
                                      ^   ^   |
                          watch(Option |   |   | mpsc(64)
                   (focused, latest)   |   |   | (LED cmds)
                                       |   |   v
+--------+                          +--+   +--------+
| KWin   |                          |config | HID   |
| tracker|                          |watcher| thread |
+--------+                          +-------+-------+
                                       mpsc(4)
+--------+
| tray   |---TrayAction(Quit)--->engine
+--------+    mpsc(4)
```

The udev monitor is a separate `std::thread` because `udev::MonitorSocket` is not `Send`. It watches for hidraw device add/remove events matching the PCPanel VID/PID and forwards them to the HID thread.

The HID thread is a `std::thread` that manages the device lifecycle: open, init, read loop, reconnect on disconnect. It uses the udev events to know when to retry device open without polling.

The PulseAudio thread is a `std::thread` running `libpulse`'s threaded mainloop. It subscribes to sink, source, sink-input, and server events, polls for state snapshots when changes are detected (20ms poll interval with dirty flag), and executes volume/mute commands sent by the engine.

## Channel map

| Channel | Type | Bound | Direction | Overflow |
|---------|------|-------|-----------|----------|
| HID positions | `watch<[u8; 9]>` | 1 (latest) | HID thread -> engine | Replaced (only latest matters) |
| HID buttons | `tokio mpsc<ButtonEvent>` | 32 | HID thread -> engine | Bounded, `blocking_send` |
| HID commands | `tokio mpsc<HidCommand>` | 64 | engine -> HID thread | Bounded |
| Audio commands | `tokio mpsc<AudioCommand>` | 32 | engine -> PA thread | Bounded |
| Audio notifications | `tokio mpsc<AudioNotification>` | 32 | PA thread -> engine | Bounded, `blocking_send` |
| Device events | `std sync_channel<DeviceEvent>` | 4 | udev thread -> HID thread | Bounded, `try_send` drops newest |
| IPC requests | `tokio mpsc<IpcMessage>` | 8 | IPC server -> engine | Bounded |
| IPC replies | `tokio oneshot<IpcResponse>` | 1 | engine -> IPC server | One-shot |
| Tray actions | `tokio mpsc<TrayAction>` | 4 | tray -> engine | Bounded |
| Focused window | `watch<Option<FocusedWindowInfo>>` | 1 (latest) | KWin tracker -> engine | Replaced (only latest matters) |
| Config reload | `tokio mpsc<()>` | 4 | config watcher -> engine | Bounded |
| Config self-write | `tokio mpsc<()>` | 4 | engine -> config watcher | Bounded (suppression signal) |

Position events are inherently replaceable -- only the latest position matters. `watch` channels are used for these.

Button events are not droppable -- missing a release means stuck mute state. These use reliable bounded mpsc channels.

## Engine

The engine (`crates/pcpaneld/src/engine.rs`) is the central coordinator. It owns all mutable state:

- `AudioState` -- latest PulseAudio snapshot (sinks, sources, sink-inputs)
- `VolumeCurve` -- constructed from config's `volume_exponent`
- `HashMap<u8, SignalPipeline>` -- per-control signal processing instances
- `[u8; 9]` last positions -- for diffing against new HID position arrays
- `[Option<Volume>; 9]` last applied volumes -- for re-applying to new sink-inputs
- `Option<FocusedWindowInfo>` -- currently focused window
- `Config` -- live config, reloaded on file changes

The `run()` function is a `tokio::select!` loop with these arms:

1. **Cancellation**: breaks the loop on token cancellation (from SIGTERM/SIGINT or tray quit)
2. **HID positions**: diffs the 9-element position array, runs changed controls through their `SignalPipeline`, maps through `VolumeCurve`, resolves the audio target, sends `AudioCommand`
3. **HID buttons**: resolves the button's audio target, sends mute toggle `AudioCommand`
4. **Audio notifications**: updates `AudioState`; on new sink-inputs, re-applies last volumes for app/focused targets
5. **IPC requests**: dispatches to handler, replies via oneshot
6. **Tray actions**: `Quit` triggers cancellation
7. **Focused window**: updates the stored `FocusedWindowInfo`
8. **Config reload**: reloads from disk, rebuilds volume curve and signal pipelines

### Target resolution

When a control event arrives, the engine resolves its `AudioTarget` to concrete PulseAudio indices:

- `DefaultOutput` -> looks up the default sink by name in `AudioState`
- `DefaultInput` -> looks up the default source by name
- `App { matcher }` -> scans all sink-inputs, returns those matching the `AppMatcher`
- `FocusedApp` -> uses `FocusedWindowInfo` to match against sink-inputs (4-strategy priority: desktopFile vs flatpak_id, resourceName vs binary, desktopFile vs binary, resourceClass vs binary)

### Volume re-application

When PulseAudio reports new sink-inputs (an app starts playing audio), the engine checks each control's last applied volume. If a control targets an app or focused-app and the new sink-input matches, the engine immediately applies that volume. This ensures a slider set to 30% stays at 30% when the app restarts or a new matching stream appears.

## HID protocol

USB identifiers: VID `0x0483`, PID `0xA3C5`. Communication is via 64-byte HID reports over the hidraw kernel interface.

### Input reports

| Byte | Field | Values |
|------|-------|--------|
| 0 | Type | `0x01` = position, `0x02` = button |
| 1 | ID | Position: 0-4 (knobs), 5-8 (sliders). Button: 0-4 (knobs). |
| 2 | Value | Position: 0-255 (analog). Button: 0 = released, 1 = pressed. |

Report ID is stripped by the kernel (the device uses Report ID 0). The raw bytes start at the type field.

### Output commands

| Command | Byte[0] | Byte[1] | Payload |
|---------|---------|---------|---------|
| Init | `0x01` | -- | Triggers device to start sending reports |
| Knob LEDs | `0x05` | `0x02` | 5 x 7-byte LED slots |
| Slider label LEDs | `0x05` | `0x01` | 4 x 7-byte LED slots |
| Slider LEDs | `0x05` | `0x00` | 4 x 7-byte LED slots |
| Logo | `0x05` | `0x03` | mode, r, g, b, speed |

Each LED slot is 7 bytes: `[mode, r1, g1, b1, r2, g2, b2]`.

LED modes: `0` = off, `1` = static (uses r1/g1/b1), `2` = gradient (r1/g1/b1 to r2/g2/b2), `3` = volume gradient.

Logo modes: `0` = off, `1` = static, `2` = rainbow, `3` = breathing.

On write, Report ID `0x00` is prepended (required by hidapi for devices with Report ID 0).

### Transport abstraction

The `HidTransport` trait (`crates/pcpaneld/src/hid.rs`) abstracts device I/O:

```rust
pub trait HidTransport: Send + 'static {
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, HidError>;
    fn write(&self, data: &[u8]) -> Result<usize, HidError>;
    fn get_serial(&self) -> Option<String>;
}
```

`HidApiTransport` wraps a real `hidapi::HidDevice`. `MockHidTransport` replays scripted byte sequences for testing.

### Device lifecycle

The HID thread (`crates/pcpaneld/src/hid_thread.rs`) runs an outer reconnection loop:

1. Try to open the device
2. On failure, wait for a udev `DeviceEvent::Added` (with 5s timeout), refresh device list, retry
3. On success, send `Init` command, drain stale reports (up to 500ms), enter read loop
4. Read loop: 100ms read timeout, process events, drain outgoing LED commands non-blocking
5. On read error (disconnect), reset positions to 0, go back to step 1
6. On cancellation, send all-off LED commands (best-effort) and exit

## PulseAudio integration

The PA thread (`crates/pcpaneld/src/pulse.rs`) uses `libpulse-binding` to talk to PipeWire's PulseAudio compatibility layer.

### Reconnection

Outer loop with exponential backoff: 1s -> 2s -> 4s (capped). Sends `AudioNotification::Disconnected` on failure, `AudioNotification::Connected` on success. Resets backoff if a session lasted >30s.

### Event subscription and snapshot

Subscribes to `SINK`, `SOURCE`, `SINK_INPUT`, and `SERVER` facility changes. Uses a dirty-flag pattern: the subscribe callback sets a flag, the main poll loop (20ms interval) checks the flag and fires 4 parallel introspection queries (server info, sinks, sources, sink-inputs). When all 4 complete, sends an `AudioNotification::StateSnapshot` to the engine.

Sink-input info extracts `application.process.binary`, `application.flatpak.id`, and `application.name` from PulseAudio properties.

### Volume conversion

Volume is normalized to `[0.0, 1.0]` as a linear factor, then converted to PulseAudio's volume units via `VolumeLinear`:

```
PA volume = Volume::from(VolumeLinear(normalized))
```

This uses PA's `pa_sw_volume_from_linear` internally, which applies perceptual (cubic) weighting. Reading volumes back uses the inverse: `VolumeLinear::from(volume).0`.

## IPC protocol

### Socket

Unix stream socket at `$XDG_RUNTIME_DIR/pcpaneld.sock` (fallback: `/run/user/{uid}/pcpaneld.sock`). Created with umask `0o077` (owner-only access).

Stale socket detection on startup: the daemon tries to connect to an existing socket. If it connects, another instance is running and the daemon exits. If connection is refused, the stale socket is removed.

### Wire format

Length-prefixed JSON:

```
[4 bytes: little-endian u32 payload length][JSON payload]
```

Maximum message size: 1 MB.

### Request types

| Type | Fields | Response |
|------|--------|----------|
| `get_status` | -- | `status` with device info, PA status, mappings |
| `list_apps` | -- | `apps` with sink-input list |
| `list_devices` | -- | `devices` with combined output/input device list |
| `list_outputs` | -- | `outputs` with output device list |
| `list_inputs` | -- | `inputs` with input device list |
| `assign_dial` | `control`, `action` | `ok` or `error` |
| `assign_button` | `control`, `action` | `ok` or `error` |
| `unassign` | `control` | `ok` or `error` |
| `get_config` | -- | `config` with TOML string |
| `reload_config` | -- | `ok` or `error` |
| `shutdown` | -- | `ok` |

`assign_dial` and `assign_button` modify the config in memory and persist it to disk. The config watcher's self-write suppression prevents a redundant reload.

## KWin integration

Focused-window tracking (`crates/pcpaneld/src/kwin.rs`) works by injecting a KWin script:

1. The daemon registers a D-Bus service (`com.pcpaneld.FocusedWindow`) on the session bus
2. Writes a JavaScript KWin script to `$XDG_RUNTIME_DIR/pcpaneld-kwin.js`
3. Loads the script via `org.kde.kwin.Scripting.loadScript()` D-Bus call
4. The KWin script hooks `workspace.windowActivated` and calls back to the daemon's D-Bus `Update(desktopFile, resourceName, resourceClass)` method on every focus change
5. The daemon updates a `watch` channel that the engine reads

On shutdown, the daemon unloads the KWin script and removes the temp file.

If any step fails (no KDE, no D-Bus, KWin scripting unavailable), it logs a warning and sits idle. `FocusedApp` targets silently produce no matches.

This is currently KDE Plasma-specific. Future work includes supporting
other Wayland compositors via `wlr-foreign-toplevel-management`.

## Signal processing pipeline

Each physical control gets a `SignalPipeline` instance (`crates/pcpaneld/src/signal.rs`) configured from the `[signal]` config section. Sliders and knobs have separate parameter sets.

```
raw hw value (0-255)
        |
        v
  [endpoint bypass]  -- 0 and 255 always pass through
        |
        v
  [rolling average]  -- sliding window of N readings, outputs integer mean
        |
        v
  [delta threshold]  -- suppress if |change| < threshold
        |
        v
  [debounce]         -- suppress if elapsed < N ms
        |
        v
  filtered hw value (0-255)
        |
        v
  [volume curve]     -- (value/255)^exponent -> linear factor -> PA VolumeLinear
        |
        v
  normalized volume
```

The pipeline is stateful per-control and resets on device reconnect or config reload.

## Development setup

### Build environment

Install the C library build dependencies for your distro — see the
[README](../README.md#build-dependencies) for per-distro commands.

On immutable Fedora (Bazzite, Silverblue, Kinoite), use `just setup` to
create a distrobox, then set `PCPANELD_CARGO` — see the README for details.

### Commands

```bash
just check    # clippy + fmt (no C deps needed, works on host)
just build    # compile (needs C deps or distrobox)
just test     # run tests
just all      # check + build + test
```

All must pass with zero warnings before any change is complete.

### Logging

The daemon uses `tracing` with `RUST_LOG` env filter. Default level is `info`.

```bash
# Run with debug logging
RUST_LOG=debug pcpaneld

# Run with trace logging for a specific module
RUST_LOG=pcpaneld::engine=trace pcpaneld
```
