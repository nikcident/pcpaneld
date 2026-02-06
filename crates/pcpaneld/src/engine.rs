use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use pcpaneld_core::audio::{
    AudioState, DeviceInfo, DeviceType, SinkInfo, SinkInputInfo, SourceInfo, Volume, VolumeCurve,
};
use pcpaneld_core::config::{Config, LedConfig};
use pcpaneld_core::control::{AppProperties, AudioTarget, ButtonAction, ControlId, DialAction};
use pcpaneld_core::hid::HidCommand;
use pcpaneld_core::ipc::{DeviceStatus, IpcRequest, IpcResponse, MappingInfo};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::hid_thread::ButtonEvent;
use crate::kwin::FocusedWindowInfo;
use crate::pulse::{AudioCommand, AudioNotification};
use crate::signal::SignalPipeline;
use crate::tray::TrayAction;

/// An IPC request bundled with its reply channel.
pub struct IpcMessage {
    pub request: IpcRequest,
    pub reply_tx: oneshot::Sender<IpcResponse>,
}

/// All channel endpoints consumed by the engine.
pub struct EngineChannels {
    pub hid_position_rx: watch::Receiver<[u8; 9]>,
    pub hid_button_rx: mpsc::Receiver<ButtonEvent>,
    pub hid_cmd_tx: mpsc::Sender<HidCommand>,
    pub audio_cmd_tx: mpsc::Sender<AudioCommand>,
    pub audio_notify_rx: mpsc::Receiver<AudioNotification>,
    pub ipc_request_rx: mpsc::Receiver<IpcMessage>,
    pub tray_action_rx: mpsc::Receiver<TrayAction>,
    pub config_reload_rx: mpsc::Receiver<()>,
    pub focused_window_rx: watch::Receiver<Option<FocusedWindowInfo>>,
    pub device_connected_rx: watch::Receiver<bool>,
    pub config_self_write_tx: mpsc::Sender<()>,
}

/// Mutable state owned by the engine loop.
struct EngineState {
    config: Config,
    config_path: PathBuf,
    audio_state: AudioState,
    volume_curve: VolumeCurve,
    device_connected: bool,
    pulse_connected: bool,
    pipelines: HashMap<u8, SignalPipeline>,
    last_positions: [u8; 9],
    last_applied_volumes: [Option<Volume>; 9],
    focused_window: Option<FocusedWindowInfo>,
    dbus_session: Option<zbus::Connection>,
}

impl EngineState {
    fn new(config: Config, config_path: PathBuf) -> Self {
        let volume_curve = VolumeCurve::new(config.signal.volume_exponent);
        let mut pipelines = HashMap::new();
        rebuild_pipelines(&config, &mut pipelines);
        Self {
            config,
            config_path,
            audio_state: AudioState::default(),
            volume_curve,
            device_connected: false,
            pulse_connected: false,
            pipelines,
            last_positions: [0u8; 9],
            last_applied_volumes: [None; 9],
            focused_window: None,
            dbus_session: None,
        }
    }
}

async fn send_audio(tx: &mpsc::Sender<AudioCommand>, cmd: AudioCommand) {
    if tx.send(cmd).await.is_err() {
        debug!("audio command dropped: PA channel closed");
    }
}

async fn send_hid(tx: &mpsc::Sender<HidCommand>, cmd: HidCommand) {
    if tx.send(cmd).await.is_err() {
        debug!("HID command dropped: device channel closed");
    }
}

/// Central engine loop.
pub async fn run(
    config: Config,
    config_path: PathBuf,
    channels: EngineChannels,
    cancel: CancellationToken,
) {
    let EngineChannels {
        mut hid_position_rx,
        mut hid_button_rx,
        hid_cmd_tx,
        audio_cmd_tx,
        mut audio_notify_rx,
        mut ipc_request_rx,
        mut tray_action_rx,
        mut config_reload_rx,
        mut focused_window_rx,
        mut device_connected_rx,
        config_self_write_tx,
    } = channels;
    let mut state = EngineState::new(config, config_path);

    info!("engine started");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("engine received shutdown signal");
                break;
            }

            // HID position changes (watch channel)
            result = hid_position_rx.changed() => {
                if result.is_err() {
                    // Watch channel closed (HID thread exited)
                    continue;
                }

                let positions = *hid_position_rx.borrow();

                // Diff against last known state
                for i in 0..9u8 {
                    if positions[i as usize] != state.last_positions[i as usize] {
                        let raw = positions[i as usize];

                        // Process through signal pipeline
                        let pipeline = state.pipelines
                            .entry(i)
                            .or_insert_with(|| make_pipeline(i, &state.config));

                        if let Some(processed) = pipeline.process(raw) {
                            if let Some(vol) = handle_position_change(
                                i,
                                processed,
                                &state,
                                &audio_cmd_tx,
                            ).await {
                                state.last_applied_volumes[i as usize] = Some(vol);
                            }
                        }
                    }
                }

                state.last_positions = positions;
            }

            // Button events
            Some(event) = hid_button_rx.recv() => {
                if event.pressed {
                    handle_button_press(
                        event.button_id,
                        &mut state,
                        &audio_cmd_tx,
                    ).await;
                }
            }

            // Audio state notifications
            Some(notification) = audio_notify_rx.recv() => {
                match notification {
                    AudioNotification::Connected => {
                        state.pulse_connected = true;
                        info!("PulseAudio connected");
                    }
                    AudioNotification::Disconnected => {
                        state.pulse_connected = false;
                        state.audio_state = AudioState::default();
                        warn!("PulseAudio disconnected");
                    }
                    AudioNotification::StateSnapshot(new_audio_state) => {
                        let old_indices: HashSet<u32> =
                            state.audio_state.sink_inputs.iter().map(|si| si.index).collect();
                        let new_sink_inputs: Vec<&SinkInputInfo> = new_audio_state
                            .sink_inputs
                            .iter()
                            .filter(|si| !old_indices.contains(&si.index))
                            .collect();

                        if !new_sink_inputs.is_empty() {
                            debug!(
                                "detected {} new sink-input(s), checking for volume re-apply",
                                new_sink_inputs.len()
                            );
                            reapply_volumes_to_new_sink_inputs(
                                &new_sink_inputs,
                                &state.last_applied_volumes,
                                &state.config,
                                &audio_cmd_tx,
                                &state.focused_window,
                            )
                            .await;
                        }

                        state.audio_state = new_audio_state;
                        debug!("audio state updated: {} sink-inputs", state.audio_state.sink_inputs.len());
                    }
                }
            }

            // IPC requests
            Some(msg) = ipc_request_rx.recv() => {
                let is_reload = matches!(msg.request, IpcRequest::ReloadConfig);
                let mutates_config = matches!(
                    msg.request,
                    IpcRequest::AssignDial { .. }
                    | IpcRequest::AssignButton { .. }
                    | IpcRequest::Unassign { .. }
                    | IpcRequest::ReloadConfig
                );
                let response = handle_ipc_request(
                    msg.request,
                    &mut state,
                    &config_self_write_tx,
                    &cancel,
                ).await;
                if mutates_config && matches!(response, IpcResponse::Ok) {
                    // Clear cached volumes so stale values aren't re-applied to
                    // sink-inputs that appear after a config change.
                    state.last_applied_volumes = [None; 9];
                }
                if is_reload && matches!(response, IpcResponse::Ok) {
                    state.volume_curve = VolumeCurve::new(state.config.signal.volume_exponent);
                    rebuild_pipelines(&state.config, &mut state.pipelines);
                    send_initial_leds(&hid_cmd_tx, &state.config.leds).await;
                }
                // Client may have disconnected; reply is best-effort.
                let _ = msg.reply_tx.send(response);
            }

            // Tray actions
            Some(action) = tray_action_rx.recv() => {
                match action {
                    TrayAction::Quit => {
                        info!("quit requested from tray");
                        cancel.cancel();
                        break;
                    }
                }
            }

            // Focused window changes
            result = focused_window_rx.changed() => {
                if result.is_ok() {
                    state.focused_window = focused_window_rx.borrow().clone();
                    debug!("focused window changed: {:?}", state.focused_window);
                }
            }

            // Device connection state changes
            result = device_connected_rx.changed() => {
                if result.is_ok() {
                    let connected = *device_connected_rx.borrow();
                    state.device_connected = connected;
                    if connected {
                        info!("device connected, sending LED config");
                        for pipeline in state.pipelines.values_mut() {
                            pipeline.reset();
                        }
                        send_initial_leds(&hid_cmd_tx, &state.config.leds).await;
                    } else {
                        info!("device disconnected");
                    }
                }
            }

            // Config reload notification
            Some(()) = config_reload_rx.recv() => {
                info!("config reload triggered");
                match Config::load(&state.config_path) {
                    Ok(new_config) => {
                        state.config = new_config;
                        state.volume_curve = VolumeCurve::new(state.config.signal.volume_exponent);
                        rebuild_pipelines(&state.config, &mut state.pipelines);
                        // Clear cached volumes — control-to-target mappings may have changed.
                        state.last_applied_volumes = [None; 9];
                        send_initial_leds(&hid_cmd_tx, &state.config.leds).await;
                        info!("config reloaded successfully");
                        match state.config.to_toml() {
                            Ok(toml) => debug!("active config:\n{toml}"),
                            Err(e) => warn!("failed to serialize config for logging: {e}"),
                        }
                    }
                    Err(e) => {
                        warn!("config reload failed (keeping previous config): {e}");
                    }
                }
            }
        }
    }

    info!("engine stopped");
}

fn make_pipeline(analog_id: u8, config: &Config) -> SignalPipeline {
    if analog_id < ControlId::NUM_KNOBS {
        SignalPipeline::new(
            config.signal.knob_rolling_average,
            config.signal.knob_delta_threshold,
            config.signal.knob_debounce_ms,
        )
    } else {
        SignalPipeline::new(
            config.signal.slider_rolling_average,
            config.signal.slider_delta_threshold,
            config.signal.slider_debounce_ms,
        )
    }
}

fn rebuild_pipelines(config: &Config, pipelines: &mut HashMap<u8, SignalPipeline>) {
    pipelines.clear();
    for i in 0..ControlId::NUM_ANALOG {
        pipelines.insert(i, make_pipeline(i, config));
    }
}

async fn handle_position_change(
    analog_id: u8,
    processed_value: u8,
    state: &EngineState,
    audio_cmd_tx: &mpsc::Sender<AudioCommand>,
) -> Option<Volume> {
    let control_id = ControlId::from_analog_id(analog_id)?;
    let control_config = state.config.get_control(control_id)?;
    let dial_action = control_config.dial.as_ref()?;

    match dial_action {
        DialAction::Volume { target } => {
            let volume = state.volume_curve.hw_to_volume(processed_value);
            send_volume_command(target, volume, state, audio_cmd_tx).await;
            Some(volume)
        }
    }
}

/// Re-apply last-known volumes to newly appeared sink-inputs.
///
/// When a browser (or other app) destroys and recreates a PA sink-input
/// (e.g., pause/play, tab reload, seek), PA's stream-restore resets volume
/// to 100%. This function detects the new sink-inputs and re-applies the
/// volume the user had set via the hardware control.
async fn reapply_volumes_to_new_sink_inputs(
    new_sink_inputs: &[&SinkInputInfo],
    last_applied_volumes: &[Option<Volume>; 9],
    config: &Config,
    audio_cmd_tx: &mpsc::Sender<AudioCommand>,
    focused_window: &Option<FocusedWindowInfo>,
) {
    for analog_id in 0..ControlId::NUM_ANALOG {
        let volume = match last_applied_volumes[analog_id as usize] {
            Some(v) => v,
            None => continue,
        };

        let control_id = match ControlId::from_analog_id(analog_id) {
            Some(id) => id,
            None => continue,
        };

        let dial_action = match config
            .get_control(control_id)
            .and_then(|cc| cc.dial.as_ref())
        {
            Some(action) => action,
            None => continue,
        };

        let target = match dial_action {
            DialAction::Volume { target } => target,
        };

        match target {
            AudioTarget::App { matcher } => {
                for si in new_sink_inputs {
                    if matcher.matches(&AppProperties::from(*si)) {
                        debug!(
                            "re-applying volume {:.2} to new sink-input {} (index {})",
                            volume.get(),
                            si.name,
                            si.index
                        );
                        send_audio(
                            audio_cmd_tx,
                            AudioCommand::SinkInputVolume {
                                index: si.index,
                                volume,
                                channels: si.channels,
                            },
                        )
                        .await;
                    }
                }
            }
            AudioTarget::FocusedApp => {
                if let Some(focused) = focused_window {
                    for si in new_sink_inputs {
                        if sink_input_matches_focused(si, focused) {
                            debug!(
                                "re-applying volume {:.2} to new focused sink-input {} (index {})",
                                volume.get(),
                                si.name,
                                si.index
                            );
                            send_audio(
                                audio_cmd_tx,
                                AudioCommand::SinkInputVolume {
                                    index: si.index,
                                    volume,
                                    channels: si.channels,
                                },
                            )
                            .await;
                        }
                    }
                }
            }
            // DefaultOutput/DefaultInput target devices, not sink-inputs
            AudioTarget::DefaultOutput | AudioTarget::DefaultInput => {}
        }
    }
}

/// Lazily initialize and cache a session D-Bus connection for MPRIS commands.
///
/// The session bus connection is stable for the lifetime of a desktop session,
/// so no reconnection logic is needed.
async fn get_dbus_session(cached: &mut Option<zbus::Connection>) -> Option<&zbus::Connection> {
    if cached.is_none() {
        match zbus::Connection::session().await {
            Ok(conn) => {
                *cached = Some(conn);
            }
            Err(e) => {
                warn!("failed to connect to session D-Bus: {e}");
                return None;
            }
        }
    }
    cached.as_ref()
}

/// Spawn a shell command as a fire-and-forget subprocess.
///
/// Security model: the IPC socket is user-only (umask 0o077), the config file is
/// user-owned, so `sh -c <user_string>` runs with the daemon's own user permissions.
/// No privilege escalation is possible — any command the user could configure here,
/// they could also run directly from their shell.
///
/// Limits: at most 8 concurrent exec commands. Each command is killed after 30 seconds.
fn execute_command(command: &str) {
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    static EXEC_SEMAPHORE: Semaphore = Semaphore::const_new(8);

    let permit = match EXEC_SEMAPHORE.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("exec command dropped (concurrency limit): {command}");
            return;
        }
    };

    let command = command.to_owned();
    tokio::spawn(async move {
        let _permit = permit; // held until task completes
        match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(mut child) => {
                match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
                    Ok(Ok(status)) if !status.success() => {
                        warn!(
                            "exec command exited with {}: {command}",
                            status
                                .code()
                                .map_or("signal".to_string(), |c| c.to_string())
                        );
                    }
                    Ok(Err(e)) => {
                        warn!("failed to wait on exec command: {e}");
                    }
                    Err(_) => {
                        warn!("exec command timed out (30s), killing: {command}");
                        let _ = child.kill().await;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                warn!("failed to spawn exec command: {e}");
            }
        }
    });
}

async fn handle_button_press(
    button_id: u8,
    state: &mut EngineState,
    audio_cmd_tx: &mpsc::Sender<AudioCommand>,
) {
    let control_id = match ControlId::from_button_id(button_id) {
        Some(id) => id,
        None => return,
    };

    let media_command = {
        let control_config = match state.config.get_control(control_id) {
            Some(c) => c,
            None => return,
        };
        let button_action = match &control_config.button {
            Some(action) => action,
            None => return,
        };
        match button_action {
            ButtonAction::Mute { target } => {
                send_mute_toggle(target, state, audio_cmd_tx).await;
                return;
            }
            ButtonAction::Exec { command } => {
                execute_command(command);
                return;
            }
            ButtonAction::Media { command } => *command,
        }
    };
    // Config borrow dropped. Safe to take &mut state.dbus_session.
    if let Some(conn) = get_dbus_session(&mut state.dbus_session).await {
        if let Err(e) = crate::mpris::send_media_command(conn, media_command).await {
            warn!("MPRIS command failed: {e}");
        }
    }
}

enum ResolvedTarget<'a> {
    Sink(&'a SinkInfo),
    Source(&'a SourceInfo),
    SinkInputs(Vec<&'a SinkInputInfo>),
}

fn resolve_target<'a>(
    target: &AudioTarget,
    audio_state: &'a AudioState,
    focused_window: &Option<FocusedWindowInfo>,
) -> Option<ResolvedTarget<'a>> {
    match target {
        AudioTarget::DefaultOutput => find_default_sink(audio_state).map(ResolvedTarget::Sink),
        AudioTarget::DefaultInput => find_default_source(audio_state).map(ResolvedTarget::Source),
        AudioTarget::App { matcher } => {
            let inputs: Vec<_> = audio_state
                .sink_inputs
                .iter()
                .filter(|si| matcher.matches(&AppProperties::from(*si)))
                .collect();
            (!inputs.is_empty()).then_some(ResolvedTarget::SinkInputs(inputs))
        }
        AudioTarget::FocusedApp => {
            let focused = focused_window.as_ref()?;
            let inputs = find_focused_sink_inputs(focused, &audio_state.sink_inputs);
            (!inputs.is_empty()).then_some(ResolvedTarget::SinkInputs(inputs))
        }
    }
}

async fn send_volume_command(
    target: &AudioTarget,
    volume: Volume,
    state: &EngineState,
    audio_cmd_tx: &mpsc::Sender<AudioCommand>,
) {
    let resolved = match resolve_target(target, &state.audio_state, &state.focused_window) {
        Some(r) => r,
        None => return,
    };
    match resolved {
        ResolvedTarget::Sink(sink) => {
            send_audio(
                audio_cmd_tx,
                AudioCommand::SinkVolume {
                    index: sink.index,
                    volume,
                    channels: sink.channels,
                },
            )
            .await;
        }
        ResolvedTarget::Source(source) => {
            send_audio(
                audio_cmd_tx,
                AudioCommand::SourceVolume {
                    index: source.index,
                    volume,
                    channels: source.channels,
                },
            )
            .await;
        }
        ResolvedTarget::SinkInputs(inputs) => {
            for si in inputs {
                send_audio(
                    audio_cmd_tx,
                    AudioCommand::SinkInputVolume {
                        index: si.index,
                        volume,
                        channels: si.channels,
                    },
                )
                .await;
            }
        }
    }
}

async fn send_mute_toggle(
    target: &AudioTarget,
    state: &EngineState,
    audio_cmd_tx: &mpsc::Sender<AudioCommand>,
) {
    let resolved = match resolve_target(target, &state.audio_state, &state.focused_window) {
        Some(r) => r,
        None => return,
    };
    match resolved {
        ResolvedTarget::Sink(sink) => {
            send_audio(
                audio_cmd_tx,
                AudioCommand::SinkMute {
                    index: sink.index,
                    mute: !sink.muted,
                },
            )
            .await;
        }
        ResolvedTarget::Source(source) => {
            send_audio(
                audio_cmd_tx,
                AudioCommand::SourceMute {
                    index: source.index,
                    mute: !source.muted,
                },
            )
            .await;
        }
        ResolvedTarget::SinkInputs(inputs) => {
            for si in inputs {
                send_audio(
                    audio_cmd_tx,
                    AudioCommand::SinkInputMute {
                        index: si.index,
                        mute: !si.muted,
                    },
                )
                .await;
            }
        }
    }
}

/// Extract the stem from a desktop file ID: the substring after the last `.`,
/// or the whole string if there are no dots.
///
/// Examples: `"org.mozilla.firefox"` → `"firefox"`, `"firefox"` → `"firefox"`.
fn desktop_file_stem(s: &str) -> &str {
    match s.rfind('.') {
        Some(pos) => &s[pos + 1..],
        None => s,
    }
}

/// Strip file extension and well-known wrapper suffixes from a binary name.
///
/// First removes a file extension (`.xxx`), then strips wrapper suffixes
/// (`-bin`, `-wrapped`) that distros/Nix add. Each step is applied at most once.
///
/// Examples: `"vlc.bin"` → `"vlc"`, `"firefox-bin"` → `"firefox"`,
/// `"ptyxis-wrapped"` → `"ptyxis"`, `"spotify"` → `"spotify"`,
/// `"some-app-bin"` → `"some-app"`, `"cabin"` → `"cabin"`.
fn binary_stem(s: &str) -> &str {
    // Strip file extension first (e.g., "vlc.bin" → "vlc", "app.x86_64" → "app")
    let without_ext = match s.rfind('.') {
        Some(pos) if pos > 0 => &s[..pos],
        _ => s,
    };
    // Then strip wrapper suffixes (uses strip_suffix so "cabin" is unchanged)
    without_ext
        .strip_suffix("-bin")
        .or_else(|| without_ext.strip_suffix("-wrapped"))
        .unwrap_or(without_ext)
}

/// Check if a single sink-input matches the currently focused window.
///
/// Tries matching strategies in priority order:
/// 1. `desktopFile` vs sink-input `flatpak_id` (Flatpak apps)
/// 2. `resourceName` vs sink-input `binary` (native apps)
/// 3. `desktopFile` stem vs `binary` stem (reverse-DNS desktop files, `-bin` binaries)
/// 4. `desktopFile` vs sink-input `binary` (exact fallback for native apps)
/// 5. `resourceClass` vs sink-input `binary` (another fallback)
/// 6. Direct PID match (Wine/Proton games where binary is `wine64-preloader`)
fn sink_input_matches_focused(si: &SinkInputInfo, focused: &FocusedWindowInfo) -> bool {
    let eq_ci = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

    // Strategy 1: desktopFile vs flatpak_id
    matches!(
        (&focused.desktop_file, &si.flatpak_id),
        (Some(df), Some(fi)) if eq_ci(df, fi)
    )
    // Strategy 2: resourceName vs binary
    || matches!(
        (&focused.resource_name, &si.binary),
        (Some(rn), Some(bin)) if eq_ci(rn, bin)
    )
    // Strategy 3: desktopFile stem vs binary stem
    // Handles reverse-DNS desktop files (org.mozilla.firefox) matched against
    // binaries with wrapper suffixes (firefox-bin).
    || matches!(
        (&focused.desktop_file, &si.binary),
        (Some(df), Some(bin)) if eq_ci(desktop_file_stem(df), binary_stem(bin))
    )
    // Strategy 4: desktopFile vs binary (exact)
    || matches!(
        (&focused.desktop_file, &si.binary),
        (Some(df), Some(bin)) if eq_ci(df, bin)
    )
    // Strategy 5: resourceClass vs binary
    || matches!(
        (&focused.resource_class, &si.binary),
        (Some(rc), Some(bin)) if eq_ci(rc, bin)
    )
    // Strategy 6: direct PID match — Wine/Proton games report a generic binary
    // (wine64-preloader) but the window PID matches the audio stream PID.
    || matches!(
        (focused.pid, si.pid),
        (Some(wp), Some(sp)) if wp == sp
    )
}

/// Find sink-inputs that belong to the currently focused window.
///
/// De-duplicates results by sink-input index so each gets at most one command.
fn find_focused_sink_inputs<'a>(
    focused: &FocusedWindowInfo,
    sink_inputs: &'a [SinkInputInfo],
) -> Vec<&'a SinkInputInfo> {
    let mut seen = HashSet::new();
    let mut results = Vec::new();

    for si in sink_inputs {
        if seen.contains(&si.index) {
            continue;
        }

        if sink_input_matches_focused(si, focused) {
            seen.insert(si.index);
            results.push(si);
        }
    }

    results
}

fn find_default_sink(state: &AudioState) -> Option<&SinkInfo> {
    let default_name = state.default_sink_name.as_deref()?;
    state.sinks.iter().find(|s| s.name == default_name)
}

fn find_default_source(state: &AudioState) -> Option<&SourceInfo> {
    let default_name = state.default_source_name.as_deref()?;
    state.sources.iter().find(|s| s.name == default_name)
}

/// Save config and notify the watcher to suppress redundant reload.
async fn save_and_notify(
    state: &EngineState,
    config_self_write_tx: &mpsc::Sender<()>,
) -> IpcResponse {
    if let Err(e) = state.config.save(&state.config_path) {
        return IpcResponse::Error {
            message: format!("failed to save config: {e}"),
        };
    }
    // Config watcher will detect the write independently; this signal just avoids
    // a redundant reload. Non-critical if dropped.
    let _ = config_self_write_tx.send(()).await;
    IpcResponse::Ok
}

async fn handle_ipc_request(
    request: IpcRequest,
    state: &mut EngineState,
    config_self_write_tx: &mpsc::Sender<()>,
    cancel: &CancellationToken,
) -> IpcResponse {
    match request {
        IpcRequest::GetStatus => {
            let mappings = build_mapping_info(&state.config);
            IpcResponse::Status {
                device: DeviceStatus {
                    connected: state.device_connected,
                    serial: state.config.device.serial.clone(),
                },
                pulse_connected: state.pulse_connected,
                mappings,
            }
        }
        IpcRequest::ListApps => IpcResponse::Apps {
            apps: state.audio_state.sink_inputs.clone(),
        },
        IpcRequest::ListDevices => {
            let devices: Vec<DeviceInfo> = state
                .audio_state
                .sinks
                .iter()
                .map(|s| DeviceInfo {
                    device_type: DeviceType::Output,
                    index: s.index,
                    name: s.name.clone(),
                    description: s.description.clone(),
                    volume: s.volume,
                    muted: s.muted,
                })
                .chain(state.audio_state.sources.iter().map(|s| DeviceInfo {
                    device_type: DeviceType::Input,
                    index: s.index,
                    name: s.name.clone(),
                    description: s.description.clone(),
                    volume: s.volume,
                    muted: s.muted,
                }))
                .collect();
            IpcResponse::Devices { devices }
        }
        IpcRequest::ListOutputs => IpcResponse::Outputs {
            outputs: state.audio_state.sinks.clone(),
        },
        IpcRequest::ListInputs => IpcResponse::Inputs {
            inputs: state.audio_state.sources.clone(),
        },
        IpcRequest::AssignDial { control, action } => {
            let entry = state
                .config
                .controls
                .entry(control.config_key())
                .or_default();
            entry.dial = Some(action);
            save_and_notify(state, config_self_write_tx).await
        }
        IpcRequest::AssignButton { control, action } => {
            let entry = state
                .config
                .controls
                .entry(control.config_key())
                .or_default();
            entry.button = Some(action);
            save_and_notify(state, config_self_write_tx).await
        }
        IpcRequest::Unassign { control } => {
            state.config.remove_control(control);
            save_and_notify(state, config_self_write_tx).await
        }
        IpcRequest::GetConfig => match state.config.to_toml() {
            Ok(toml) => IpcResponse::Config { toml },
            Err(e) => IpcResponse::Error {
                message: format!("failed to serialize config: {e}"),
            },
        },
        IpcRequest::ReloadConfig => match Config::load(&state.config_path) {
            Ok(new_config) => {
                state.config = new_config;
                match state.config.to_toml() {
                    Ok(toml) => debug!("active config:\n{toml}"),
                    Err(e) => warn!("failed to serialize config for logging: {e}"),
                }
                IpcResponse::Ok
            }
            Err(e) => IpcResponse::Error {
                message: format!("failed to reload config: {e}"),
            },
        },
        IpcRequest::Shutdown => {
            cancel.cancel();
            IpcResponse::Ok
        }
    }
}

fn build_mapping_info(config: &Config) -> Vec<MappingInfo> {
    let mut mappings = Vec::new();

    for analog_id in 0..ControlId::NUM_ANALOG {
        let Some(control_id) = ControlId::from_analog_id(analog_id) else {
            continue;
        };
        if let Some(cc) = config.get_control(control_id) {
            let dial = cc.dial.as_ref().map(|d| match d {
                DialAction::Volume { target } => format!("volume {target}"),
            });
            let button = cc.button.as_ref().map(|b| match b {
                ButtonAction::Mute { target } => format!("mute {target}"),
                ButtonAction::Media { command } => format!("media {command:?}"),
                ButtonAction::Exec { command } => format!("exec {command}"),
            });
            if dial.is_some() || button.is_some() {
                mappings.push(MappingInfo {
                    control: control_id.config_key(),
                    dial,
                    button,
                });
            }
        }
    }

    mappings
}

async fn send_initial_leds(hid_cmd_tx: &mpsc::Sender<HidCommand>, led_config: &LedConfig) {
    use pcpaneld_core::hid::{LedSlot, LogoMode};

    let knob_leds = if led_config.knobs {
        [LedSlot::static_color(255, 255, 255); 5]
    } else {
        [LedSlot::OFF; 5]
    };

    let slider_leds = if led_config.sliders {
        [LedSlot::static_color(0, 100, 255); 4]
    } else {
        [LedSlot::OFF; 4]
    };

    let slider_label_leds = if led_config.slider_labels {
        [LedSlot::static_color(0, 100, 255); 4]
    } else {
        [LedSlot::OFF; 4]
    };

    send_hid(hid_cmd_tx, HidCommand::SetKnobLeds(knob_leds)).await;
    send_hid(
        hid_cmd_tx,
        HidCommand::SetSliderLabelLeds(slider_label_leds),
    )
    .await;
    send_hid(hid_cmd_tx, HidCommand::SetSliderLeds(slider_leds)).await;

    let logo_cmd = if led_config.logo {
        HidCommand::SetLogo {
            mode: LogoMode::Static,
            r: 255,
            g: 255,
            b: 255,
            speed: 0,
        }
    } else {
        HidCommand::SetLogo {
            mode: LogoMode::Static,
            r: 0,
            g: 0,
            b: 0,
            speed: 0,
        }
    };
    send_hid(hid_cmd_tx, logo_cmd).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcpaneld_core::control::{AppMatcher, ControlConfig};

    fn make_sink_input(
        index: u32,
        name: &str,
        binary: Option<&str>,
        flatpak_id: Option<&str>,
    ) -> SinkInputInfo {
        SinkInputInfo {
            index,
            name: name.to_string(),
            binary: binary.map(String::from),
            flatpak_id: flatpak_id.map(String::from),
            pid: None,
            sink_index: 0,
            volume: Volume::new(0.5),
            muted: false,
            channels: 2,
        }
    }

    fn make_focused(
        desktop_file: Option<&str>,
        resource_name: Option<&str>,
        resource_class: Option<&str>,
    ) -> FocusedWindowInfo {
        FocusedWindowInfo {
            desktop_file: desktop_file.map(String::from),
            resource_name: resource_name.map(String::from),
            resource_class: resource_class.map(String::from),
            pid: None,
        }
    }

    #[test]
    fn focused_matches_desktop_file_vs_flatpak_id() {
        let focused = make_focused(Some("org.mozilla.firefox"), None, None);
        let inputs = [make_sink_input(
            1,
            "Firefox",
            Some("firefox"),
            Some("org.mozilla.firefox"),
        )];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].index, 1);
    }

    #[test]
    fn focused_matches_resource_name_vs_binary() {
        let focused = make_focused(None, Some("firefox"), None);
        let inputs = [make_sink_input(1, "Firefox", Some("firefox"), None)];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].index, 1);
    }

    #[test]
    fn focused_matches_desktop_file_vs_binary_fallback() {
        // Native app where desktopFile matches binary name
        let focused = make_focused(Some("firefox"), None, None);
        let inputs = [make_sink_input(1, "Firefox", Some("firefox"), None)];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].index, 1);
    }

    #[test]
    fn focused_matches_resource_class_vs_binary_fallback() {
        let focused = make_focused(None, None, Some("Firefox"));
        let inputs = [make_sink_input(1, "Firefox", Some("firefox"), None)];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].index, 1);
    }

    #[test]
    fn focused_no_match_returns_empty() {
        let focused = make_focused(Some("org.gnome.Ptyxis"), Some("ptyxis"), Some("Ptyxis"));
        let inputs = [make_sink_input(1, "Firefox", Some("firefox"), None)];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert!(matched.is_empty());
    }

    #[test]
    fn focused_deduplicates_multi_strategy_matches() {
        // A sink-input that matches on BOTH desktopFile-vs-flatpak_id AND
        // resourceName-vs-binary should only appear once.
        let focused = make_focused(
            Some("org.mozilla.firefox"),
            Some("firefox"),
            Some("Firefox"),
        );
        let inputs = [make_sink_input(
            1,
            "Firefox",
            Some("firefox"),
            Some("org.mozilla.firefox"),
        )];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn focused_case_insensitive_across_all_strategies() {
        // Strategy 1: case-insensitive desktopFile vs flatpak_id
        let focused = make_focused(Some("Org.Mozilla.Firefox"), None, None);
        let inputs = [make_sink_input(
            1,
            "Firefox",
            None,
            Some("org.mozilla.firefox"),
        )];
        assert_eq!(find_focused_sink_inputs(&focused, &inputs).len(), 1);

        // Strategy 2: case-insensitive resourceName vs binary
        let focused = make_focused(None, Some("Firefox"), None);
        let inputs = [make_sink_input(2, "Firefox", Some("firefox"), None)];
        assert_eq!(find_focused_sink_inputs(&focused, &inputs).len(), 1);

        // Strategy 3: case-insensitive desktopFile stem vs binary stem
        let focused = make_focused(Some("Org.Mozilla.Firefox"), None, None);
        let inputs = [make_sink_input(3, "Firefox", Some("firefox-bin"), None)];
        assert_eq!(find_focused_sink_inputs(&focused, &inputs).len(), 1);

        // Strategy 4: case-insensitive desktopFile vs binary (exact)
        let focused = make_focused(Some("Firefox"), None, None);
        let inputs = [make_sink_input(4, "Firefox", Some("firefox"), None)];
        assert_eq!(find_focused_sink_inputs(&focused, &inputs).len(), 1);

        // Strategy 5: case-insensitive resourceClass vs binary
        let focused = make_focused(None, None, Some("FIREFOX"));
        let inputs = [make_sink_input(5, "Firefox", Some("firefox"), None)];
        assert_eq!(find_focused_sink_inputs(&focused, &inputs).len(), 1);
    }

    #[test]
    fn focused_matches_multiple_sink_inputs_for_same_app() {
        // Multiple instances of same app should all match
        let focused = make_focused(Some("firefox"), Some("firefox"), None);
        let inputs = [
            make_sink_input(1, "Firefox", Some("firefox"), None),
            make_sink_input(2, "Firefox - YouTube", Some("firefox"), None),
        ];
        let matched = find_focused_sink_inputs(&focused, &inputs);
        assert_eq!(matched.len(), 2);
    }

    // --- sink_input_matches_focused tests ---

    #[test]
    fn sink_input_matches_focused_all_strategies() {
        let si_flatpak = make_sink_input(1, "Firefox", None, Some("org.mozilla.firefox"));
        let si_binary = make_sink_input(2, "Firefox", Some("firefox"), None);
        let si_bin_suffix = make_sink_input(3, "Firefox", Some("firefox-bin"), None);

        // Strategy 1: desktopFile vs flatpak_id
        let focused = make_focused(Some("org.mozilla.firefox"), None, None);
        assert!(sink_input_matches_focused(&si_flatpak, &focused));

        // Strategy 2: resourceName vs binary
        let focused = make_focused(None, Some("firefox"), None);
        assert!(sink_input_matches_focused(&si_binary, &focused));

        // Strategy 3: desktopFile stem vs binary stem
        let focused = make_focused(Some("org.mozilla.firefox"), None, None);
        assert!(sink_input_matches_focused(&si_bin_suffix, &focused));

        // Strategy 4: desktopFile vs binary (exact)
        let focused = make_focused(Some("firefox"), None, None);
        assert!(sink_input_matches_focused(&si_binary, &focused));

        // Strategy 5: resourceClass vs binary
        let focused = make_focused(None, None, Some("firefox"));
        assert!(sink_input_matches_focused(&si_binary, &focused));
    }

    // --- PID matching (strategy 6) tests ---

    #[test]
    fn focused_matches_by_pid_when_no_string_match() {
        // Wine/Proton scenario: generic binary, no desktop file match, but PIDs match
        let mut focused = make_focused(Some("steam_app_12345"), None, Some("steam_app_12345"));
        focused.pid = Some(9876);
        let mut si = make_sink_input(1, "Game Audio", Some("wine64-preloader"), None);
        si.pid = Some(9876);
        assert!(sink_input_matches_focused(&si, &focused));
    }

    #[test]
    fn focused_no_match_when_pids_differ() {
        let mut focused = make_focused(Some("steam_app_12345"), None, None);
        focused.pid = Some(9876);
        let mut si = make_sink_input(1, "Game Audio", Some("wine64-preloader"), None);
        si.pid = Some(5555);
        assert!(!sink_input_matches_focused(&si, &focused));
    }

    #[test]
    fn focused_no_match_when_focused_pid_none() {
        let focused = make_focused(Some("steam_app_12345"), None, None);
        // focused.pid is None
        let mut si = make_sink_input(1, "Game Audio", Some("wine64-preloader"), None);
        si.pid = Some(1234);
        assert!(!sink_input_matches_focused(&si, &focused));
    }

    #[test]
    fn focused_no_match_when_si_pid_none() {
        let mut focused = make_focused(Some("steam_app_12345"), None, None);
        focused.pid = Some(1234);
        let si = make_sink_input(1, "Game Audio", Some("wine64-preloader"), None);
        // si.pid is None
        assert!(!sink_input_matches_focused(&si, &focused));
    }

    #[test]
    fn string_match_still_works_when_pids_differ() {
        let mut focused = make_focused(None, Some("firefox"), None);
        focused.pid = Some(1111);
        let mut si = make_sink_input(1, "Firefox", Some("firefox"), None);
        si.pid = Some(2222);
        // String strategy 2 matches, even though PIDs differ
        assert!(sink_input_matches_focused(&si, &focused));
    }

    // --- desktop_file_stem / binary_stem helper tests ---

    #[test]
    fn desktop_file_stem_extracts_last_segment() {
        assert_eq!(desktop_file_stem("org.mozilla.firefox"), "firefox");
        assert_eq!(desktop_file_stem("com.spotify.Client"), "Client");
    }

    #[test]
    fn desktop_file_stem_returns_whole_string_without_dots() {
        assert_eq!(desktop_file_stem("firefox"), "firefox");
    }

    #[test]
    fn binary_stem_strips_wrapper_suffixes() {
        assert_eq!(binary_stem("firefox-bin"), "firefox");
        assert_eq!(binary_stem("some-app-bin"), "some-app");
        assert_eq!(binary_stem("ptyxis-wrapped"), "ptyxis");
    }

    #[test]
    fn binary_stem_strips_file_extension() {
        assert_eq!(binary_stem("vlc.bin"), "vlc");
        assert_eq!(binary_stem("app.x86_64"), "app");
    }

    #[test]
    fn binary_stem_strips_extension_then_wrapper() {
        // Extension stripped first, then wrapper suffix
        assert_eq!(binary_stem("foo-bin.exe"), "foo");
    }

    #[test]
    fn binary_stem_returns_whole_string_without_suffix() {
        assert_eq!(binary_stem("spotify"), "spotify");
        // "cabin" must not be mangled — strip_suffix is exact, not trim
        assert_eq!(binary_stem("cabin"), "cabin");
    }

    // --- stem matching strategy integration test ---

    #[test]
    fn focused_matches_via_stem_strategy() {
        // Firefox: reverse-DNS desktop file, binary with -bin suffix, no flatpak_id
        let focused = make_focused(
            Some("org.mozilla.firefox"),
            None,
            Some("org.mozilla.firefox"),
        );
        let si = make_sink_input(1, "Firefox", Some("firefox-bin"), None);
        assert!(sink_input_matches_focused(&si, &focused));

        // Ptyxis: case-insensitive stem match
        let focused = make_focused(Some("org.gnome.Ptyxis"), None, None);
        let si = make_sink_input(2, "Ptyxis", Some("ptyxis"), None);
        assert!(sink_input_matches_focused(&si, &focused));

        // VLC: stem match across extension + case difference
        let focused = make_focused(Some("org.videolan.VLC"), None, None);
        let si = make_sink_input(3, "VLC media player", Some("vlc.bin"), None);
        assert!(sink_input_matches_focused(&si, &focused));
    }

    // --- reapply_volumes_to_new_sink_inputs tests ---

    fn make_config_with_app_volume(analog_id: u8, matcher: AppMatcher) -> Config {
        let mut config = Config::default();
        let control_id = ControlId::from_analog_id(analog_id).unwrap();
        config.set_control(
            control_id,
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::App { matcher },
                }),
                button: None,
            },
        );
        config
    }

    fn make_config_with_target(analog_id: u8, target: AudioTarget) -> Config {
        let mut config = Config::default();
        let control_id = ControlId::from_analog_id(analog_id).unwrap();
        config.set_control(
            control_id,
            ControlConfig {
                dial: Some(DialAction::Volume { target }),
                button: None,
            },
        );
        config
    }

    #[tokio::test]
    async fn reapply_sends_volume_for_matching_app() {
        let (tx, mut rx) = mpsc::channel(16);
        let si = make_sink_input(42, "Firefox", Some("firefox"), None);
        let new_inputs = vec![&si];
        let mut volumes: [Option<Volume>; 9] = [None; 9];
        volumes[0] = Some(Volume::new(0.3));
        let config = make_config_with_app_volume(
            0,
            AppMatcher {
                binary: Some("firefox".into()),
                ..Default::default()
            },
        );

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &None).await;

        let cmd = rx.try_recv().expect("expected a volume command");
        match cmd {
            AudioCommand::SinkInputVolume {
                index,
                volume,
                channels,
            } => {
                assert_eq!(index, 42);
                assert!((volume.get() - 0.3).abs() < f64::EPSILON);
                assert_eq!(channels, 2);
            }
            other => panic!("expected SinkInputVolume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reapply_skips_non_matching_app() {
        let (tx, mut rx) = mpsc::channel(16);
        let si = make_sink_input(42, "Chrome", Some("chrome"), None);
        let new_inputs = vec![&si];
        let mut volumes: [Option<Volume>; 9] = [None; 9];
        volumes[0] = Some(Volume::new(0.3));
        let config = make_config_with_app_volume(
            0,
            AppMatcher {
                binary: Some("firefox".into()),
                ..Default::default()
            },
        );

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &None).await;

        assert!(
            rx.try_recv().is_err(),
            "no command should be sent for non-matching app"
        );
    }

    #[tokio::test]
    async fn no_reapply_when_no_volumes_recorded() {
        let (tx, mut rx) = mpsc::channel(16);
        let si = make_sink_input(42, "Firefox", Some("firefox"), None);
        let new_inputs = vec![&si];
        let volumes: [Option<Volume>; 9] = [None; 9];
        let config = make_config_with_app_volume(
            0,
            AppMatcher {
                binary: Some("firefox".into()),
                ..Default::default()
            },
        );

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &None).await;

        assert!(
            rx.try_recv().is_err(),
            "no command should be sent when volumes are all None"
        );
    }

    #[tokio::test]
    async fn reapply_focused_app_when_focused() {
        let (tx, mut rx) = mpsc::channel(16);
        let si = make_sink_input(42, "Firefox", Some("firefox"), None);
        let new_inputs = vec![&si];
        let mut volumes: [Option<Volume>; 9] = [None; 9];
        volumes[0] = Some(Volume::new(0.5));
        let config = make_config_with_target(0, AudioTarget::FocusedApp);
        let focused = Some(make_focused(None, Some("firefox"), None));

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &focused).await;

        let cmd = rx.try_recv().expect("expected a volume command");
        match cmd {
            AudioCommand::SinkInputVolume {
                index,
                volume,
                channels,
            } => {
                assert_eq!(index, 42);
                assert!((volume.get() - 0.5).abs() < f64::EPSILON);
                assert_eq!(channels, 2);
            }
            other => panic!("expected SinkInputVolume, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reapply_focused_app_skips_when_not_focused() {
        let (tx, mut rx) = mpsc::channel(16);
        let si = make_sink_input(42, "Firefox", Some("firefox"), None);
        let new_inputs = vec![&si];
        let mut volumes: [Option<Volume>; 9] = [None; 9];
        volumes[0] = Some(Volume::new(0.5));
        let config = make_config_with_target(0, AudioTarget::FocusedApp);
        // Focused window is a different app
        let focused = Some(make_focused(None, Some("ptyxis"), None));

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &focused).await;

        assert!(
            rx.try_recv().is_err(),
            "no command should be sent when focused window doesn't match"
        );
    }

    #[tokio::test]
    async fn reapply_skips_default_output_and_input() {
        let (tx, mut rx) = mpsc::channel(16);
        let si = make_sink_input(42, "Firefox", Some("firefox"), None);
        let new_inputs = vec![&si];
        let mut volumes: [Option<Volume>; 9] = [None; 9];
        volumes[0] = Some(Volume::new(0.5));
        volumes[1] = Some(Volume::new(0.7));

        let mut config = Config::default();
        config.set_control(
            ControlId::Knob(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: None,
            },
        );
        config.set_control(
            ControlId::Knob(1),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultInput,
                }),
                button: None,
            },
        );

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &None).await;

        assert!(
            rx.try_recv().is_err(),
            "DefaultOutput/DefaultInput should never trigger sink-input re-apply"
        );
    }

    #[tokio::test]
    async fn reapply_empty_new_inputs_does_nothing() {
        let (tx, mut rx) = mpsc::channel(16);
        let new_inputs: Vec<&SinkInputInfo> = vec![];
        let mut volumes: [Option<Volume>; 9] = [None; 9];
        volumes[0] = Some(Volume::new(0.5));
        let config = make_config_with_app_volume(
            0,
            AppMatcher {
                binary: Some("firefox".into()),
                ..Default::default()
            },
        );

        reapply_volumes_to_new_sink_inputs(&new_inputs, &volumes, &config, &tx, &None).await;

        assert!(
            rx.try_recv().is_err(),
            "no commands should be sent when there are no new sink-inputs"
        );
    }

    // --- engine integration test ---

    /// End-to-end test: HID position change → signal pipeline → volume curve → audio command.
    ///
    /// Spawns the real engine loop, feeds it an audio state snapshot and HID position
    /// change, then asserts the correct `AudioCommand::SinkInputVolume` emerges.
    #[tokio::test]
    async fn engine_hid_position_produces_audio_command() {
        use pcpaneld_core::audio::VolumeCurve;

        let cancel = CancellationToken::new();

        // --- Create ALL channel pairs for EngineChannels ---
        let (hid_position_tx, hid_position_rx) = watch::channel([0u8; 9]);
        let (_hid_button_tx, hid_button_rx) = mpsc::channel(4);
        let (hid_cmd_tx, _hid_cmd_rx) = mpsc::channel(4);
        let (audio_cmd_tx, mut audio_cmd_rx) = mpsc::channel(32);
        let (audio_notify_tx, audio_notify_rx) = mpsc::channel(32);
        let (_ipc_request_tx, ipc_request_rx) = mpsc::channel(4);
        let (_tray_action_tx, tray_action_rx) = mpsc::channel(4);
        let (_config_reload_tx, config_reload_rx) = mpsc::channel(4);
        let (_focused_window_tx, focused_window_rx) =
            watch::channel::<Option<FocusedWindowInfo>>(None);
        let (_device_connected_tx, device_connected_rx) = watch::channel(false);
        let (config_self_write_tx, _config_self_write_rx) = mpsc::channel(4);

        // --- Build config with knob1 → app volume for "firefox" ---
        let config = make_config_with_app_volume(
            0,
            AppMatcher {
                binary: Some("firefox".into()),
                ..Default::default()
            },
        );

        let config_path = PathBuf::from("/nonexistent/test-config.toml");

        let channels = EngineChannels {
            hid_position_rx,
            hid_button_rx,
            hid_cmd_tx,
            audio_cmd_tx,
            audio_notify_rx,
            ipc_request_rx,
            tray_action_rx,
            config_reload_rx,
            focused_window_rx,
            device_connected_rx,
            config_self_write_tx,
        };

        // --- Spawn engine ---
        let engine_cancel = cancel.clone();
        let engine_handle = tokio::spawn(async move {
            run(config, config_path, channels, engine_cancel).await;
        });

        // --- Seed audio state with a matching sink-input ---
        let audio_state = AudioState {
            sink_inputs: vec![make_sink_input(42, "Firefox", Some("firefox"), None)],
            ..Default::default()
        };
        audio_notify_tx
            .send(AudioNotification::StateSnapshot(audio_state))
            .await
            .unwrap();

        // Wait for engine to process the snapshot. There's no observable side effect
        // to poll for — the engine silently updates internal state.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // --- Send HID position change for knob1 (analog_id 0) ---
        // Signal pipeline with default knob params (window=3, delta=1, debounce=0)
        // will let 128 through on first value since last_emitted is None.
        let mut positions = [0u8; 9];
        positions[0] = 128;
        hid_position_tx.send(positions).unwrap();

        // --- Assert audio command with timeout ---
        let cmd = tokio::time::timeout(std::time::Duration::from_secs(2), audio_cmd_rx.recv())
            .await
            .expect("timed out waiting for audio command")
            .expect("audio channel closed unexpectedly");

        let expected_volume = VolumeCurve::new(1.0).hw_to_volume(128);
        match cmd {
            AudioCommand::SinkInputVolume {
                index,
                volume,
                channels,
            } => {
                assert_eq!(index, 42, "should target sink-input index 42");
                assert!(
                    (volume.get() - expected_volume.get()).abs() < f64::EPSILON,
                    "volume mismatch: got {}, expected {}",
                    volume.get(),
                    expected_volume.get()
                );
                assert_eq!(channels, 2, "sink-input has 2 channels");
            }
            other => panic!("expected SinkInputVolume, got {other:?}"),
        }

        // --- Clean up ---
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), engine_handle).await;
    }
}
