use std::path::PathBuf;

use anyhow::{Context, Result};
use pcpaneld_core::config::{self, Config};
use pcpaneld_core::ipc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{config_watcher, engine, hid_thread, ipc_server, kwin, pulse, tray};

/// Run the daemon with the given log level.
pub fn run(log_level: &str) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();

    info!("pcpaneld v{} starting", env!("CARGO_PKG_VERSION"));

    let config_path = Config::default_path().expect("failed to resolve XDG config directory");
    match config::bootstrap_config(&config_path) {
        Ok(true) => info!("created default config at {}", config_path.display()),
        Ok(false) => {}
        Err(e) => warn!("failed to bootstrap config: {e}"),
    }

    let config = Config::load(&config_path).context("failed to load config")?;
    info!("loaded config from {}", config_path.display());
    match config.to_toml() {
        Ok(toml) => info!("active config:\n{toml}"),
        Err(e) => warn!("failed to serialize config for logging: {e}"),
    }

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let result = rt.block_on(async_main(config, config_path));
    // Explicit shutdown with timeout: HID, PulseAudio, and udev threads use blocking
    // APIs. The default runtime drop would wait for them indefinitely.
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

async fn async_main(config: Config, config_path: PathBuf) -> Result<()> {
    let cancel = CancellationToken::new();

    let socket_path = ipc::default_socket_path();
    ipc_server::cleanup_stale_socket(&socket_path).await?;

    // Set up channels
    let (hid_position_tx, hid_position_rx) = tokio::sync::watch::channel([0u8; 9]);
    let (hid_button_tx, hid_button_rx) = tokio::sync::mpsc::channel(32);
    let (hid_cmd_tx, hid_cmd_rx) = tokio::sync::mpsc::channel(64);
    let (audio_cmd_tx, audio_cmd_rx) = tokio::sync::mpsc::channel(32);
    let (audio_notify_tx, audio_notify_rx) = tokio::sync::mpsc::channel(32);
    let (ipc_request_tx, ipc_request_rx) = tokio::sync::mpsc::channel(8);
    let (tray_action_tx, tray_action_rx) = tokio::sync::mpsc::channel(4);
    let (device_event_tx, device_event_rx) = std::sync::mpsc::sync_channel(4);
    let (device_connected_tx, device_connected_rx) = tokio::sync::watch::channel(false);

    // Start udev monitor (std::thread — MonitorSocket is not Send)
    let udev_cancel = cancel.clone();
    let udev_join = std::thread::Builder::new()
        .name("udev".into())
        .spawn(move || {
            hid_thread::run_udev_monitor(device_event_tx, udev_cancel);
        })
        .context("failed to spawn udev thread")?;

    // Start HID thread (std::thread)
    let hid_config_serial = config.device.serial.clone();
    let hid_cancel = cancel.clone();
    let hid_join = std::thread::Builder::new()
        .name("hid".into())
        .spawn(move || {
            hid_thread::run(
                hid_config_serial,
                hid_position_tx,
                hid_button_tx,
                hid_cmd_rx,
                device_event_rx,
                device_connected_tx,
                hid_cancel,
            );
        })
        .context("failed to spawn HID thread")?;

    // Start PulseAudio thread (std::thread)
    let pa_cancel = cancel.clone();
    let pa_join = std::thread::Builder::new()
        .name("pulse".into())
        .spawn(move || {
            pulse::run(audio_cmd_rx, audio_notify_tx, pa_cancel);
        })
        .context("failed to spawn PulseAudio thread")?;

    // Start IPC server (tokio task)
    let ipc_cancel = cancel.clone();
    let ipc_handle = tokio::spawn(async move {
        if let Err(e) = ipc_server::run(socket_path.clone(), ipc_request_tx, ipc_cancel).await {
            error!("IPC server failed: {e}");
        }
    });

    // Start system tray (tokio task)
    let tray_cancel = cancel.clone();
    let tray_handle = tokio::spawn(async move {
        tray::run(tray_action_tx, tray_cancel).await;
    });

    // Start KWin focused window tracker (tokio task)
    let (focused_window_tx, focused_window_rx) =
        tokio::sync::watch::channel::<Option<kwin::FocusedWindowInfo>>(None);
    let kwin_cancel = cancel.clone();
    tokio::spawn(async move {
        kwin::run(focused_window_tx, kwin_cancel).await;
    });

    // Start config watcher (tokio task)
    let (config_reload_tx, config_reload_rx) = tokio::sync::mpsc::channel(4);
    let (config_self_write_tx, config_self_write_rx) = tokio::sync::mpsc::channel::<()>(4);
    let watcher_cancel = cancel.clone();
    let config_dir = config_path
        .parent()
        .expect("config path has no parent directory")
        .to_owned();
    let config_filename = config_path
        .file_name()
        .expect("config path has no file name")
        .to_str()
        .expect("config filename is not valid UTF-8")
        .to_owned();
    let _watcher_handle = tokio::spawn(async move {
        config_watcher::run(
            config_dir,
            config_filename,
            config_reload_tx,
            config_self_write_rx,
            watcher_cancel,
        )
        .await;
    });

    // Set up signal handling
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("failed to register SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => info!("received SIGTERM"),
            _ = sigint.recv() => info!("received SIGINT"),
        }
        signal_cancel.cancel();
    });

    // Run the engine (blocks until shutdown)
    let channels = engine::EngineChannels {
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
    engine::run(config, config_path, channels, cancel.clone()).await;

    info!("engine stopped, shutting down subsystems");
    cancel.cancel();

    // Wait for threads with timeout
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), async {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = hid_join.join();
            let _ = pa_join.join();
            let _ = udev_join.join();
        })
        .await;
    })
    .await;

    // Wait for tokio tasks
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), async {
        let _ = ipc_handle.await;
        let _ = tray_handle.await;
    })
    .await;

    // Clean up socket
    let socket_path = ipc::default_socket_path();
    let _ = tokio::fs::remove_file(&socket_path).await;

    info!("shutdown complete");
    Ok(())
}
