use std::time::Duration;

use ksni::TrayMethods;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Actions from the system tray to the engine.
#[derive(Debug, Clone)]
pub enum TrayAction {
    Quit,
}

struct PcPanelTray {
    action_tx: mpsc::Sender<TrayAction>,
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
                // ksni callbacks run on a background thread — use blocking_send
                // since we can't .await in a sync context.
                let _ = tray.action_tx.blocking_send(TrayAction::Quit);
            }),
            ..Default::default()
        })]
    }
}

/// Run the system tray.
///
/// Uses ksni for SNI (StatusNotifierItem) registration on KDE/Wayland.
pub async fn run(action_tx: mpsc::Sender<TrayAction>, cancel: CancellationToken) {
    // spawn() consumes self, so reconstruct PcPanelTray on each retry.
    // Sender::clone() is cheap (Arc increment).
    let handle = 'retry: {
        for attempt in 1..=5u64 {
            let tray = PcPanelTray {
                action_tx: action_tx.clone(),
            };
            match tray.spawn().await {
                Ok(handle) => break 'retry handle,
                Err(e) => {
                    if attempt < 5 {
                        warn!("tray spawn failed (attempt {attempt}/5): {e}");
                        tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                    } else {
                        warn!("tray spawn failed after {attempt} attempts: {e}");
                        return;
                    }
                }
            }
        }
        return;
    };

    cancel.cancelled().await;
    handle.shutdown().await;
}
