use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Actions from the system tray to the engine.
#[derive(Debug, Clone)]
pub enum TrayAction {
    Quit,
}

struct PcPanelTray {
    action_tx: std::sync::mpsc::SyncSender<TrayAction>,
}

impl ksni::Tray for PcPanelTray {
    fn id(&self) -> String {
        "pcpaneld".into()
    }

    fn title(&self) -> String {
        "PCPanel Pro".into()
    }

    fn icon_name(&self) -> String {
        "audio-volume-high".into()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![ksni::MenuItem::Standard(ksni::menu::StandardItem {
            label: "Quit".into(),
            activate: Box::new(|tray: &mut Self| {
                let _ = tray.action_tx.try_send(TrayAction::Quit);
            }),
            ..Default::default()
        })]
    }
}

/// Run the system tray.
///
/// Uses ksni for SNI (StatusNotifierItem) registration on KDE/Wayland.
pub async fn run(action_tx: mpsc::Sender<TrayAction>, cancel: CancellationToken) {
    // Bridge from sync ksni callback to async tokio channel
    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<TrayAction>(4);

    // NOTE: service.run() blocks forever. When cancel fires, this async function
    // exits and the spawn_blocking handle is dropped (detaching the thread).
    // The blocking thread leaks until process exit. This is acceptable — ksni
    // provides no external shutdown mechanism.
    let _tray_handle = tokio::task::spawn_blocking(move || {
        for attempt in 1..=5u64 {
            let tray = PcPanelTray {
                action_tx: sync_tx.clone(),
            };
            let service = ksni::TrayService::new(tray);
            let start = std::time::Instant::now();
            let _ = service.run();
            // If run() returned quickly, it likely failed to register
            if start.elapsed() < std::time::Duration::from_secs(2) {
                if attempt < 5 {
                    warn!(
                        "tray service exited early (attempt {attempt}/5), retrying in {}ms",
                        500 * attempt
                    );
                    std::thread::sleep(std::time::Duration::from_millis(500 * attempt));
                    continue;
                }
                warn!("tray service failed after {attempt} attempts, giving up");
            }
            break;
        }
    });

    // Bridge sync channel to async: a blocking thread reads from sync_rx and
    // forwards to an async mpsc, which the select! loop below consumes.
    let (async_tx, mut async_rx) = mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        while let Ok(action) = sync_rx.recv() {
            if async_tx.blocking_send(action).is_err() {
                break; // async side shut down, exit thread
            }
        }
    });

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            Some(action) = async_rx.recv() => {
                let _ = action_tx.send(action).await;
            }
        }
    }
}
