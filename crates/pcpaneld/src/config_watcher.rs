use std::path::PathBuf;
use std::time::Instant;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

/// Config file watcher using notify crate.
///
/// Watches the config directory (not the file) to catch editor rename patterns.
/// Uses timestamp-based self-write suppression.
pub async fn run(
    config_dir: PathBuf,
    config_filename: String,
    reload_tx: mpsc::Sender<()>,
    mut self_write_rx: mpsc::Receiver<()>,
    cancel: CancellationToken,
) {
    let (tx, mut rx) = mpsc::channel(16);
    let config_filename_clone = config_filename.clone();

    let mut watcher = match RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                // Only react to Create/Modify events for the config file
                let is_write_event =
                    matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));

                if is_write_event {
                    let is_config = event.paths.iter().any(|p| {
                        p.file_name()
                            .and_then(|f| f.to_str())
                            .map(|f| f == config_filename_clone)
                            .unwrap_or(false)
                    });
                    if is_config {
                        // Channel full means a reload is already pending; drop is intentional.
                        let _ = tx.blocking_send(());
                    }
                }
            }
        },
        notify::Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            error!("failed to create config file watcher: {e}");
            return;
        }
    };

    if let Err(e) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
        error!(
            "failed to watch config directory {}: {e}",
            config_dir.display()
        );
        return;
    }

    info!("watching config directory: {}", config_dir.display());

    // Self-write suppression: track recent writes by the daemon.
    // Updated by the engine when it saves config via IPC assign/unassign.
    let mut last_self_write: Option<Instant> = None;
    let suppress_window = std::time::Duration::from_millis(100);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            Some(()) = self_write_rx.recv() => {
                last_self_write = Some(Instant::now());
            }
            Some(()) = rx.recv() => {
                // Check self-write suppression
                if let Some(t) = last_self_write {
                    if t.elapsed() < suppress_window {
                        debug!("suppressing self-triggered config reload");
                        continue;
                    }
                }

                // Debounce: wait a bit for editors that do multiple writes
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                debug!("config file changed, triggering reload");
                // Engine may have shut down; non-critical if dropped.
                let _ = reload_tx.send(()).await;
            }
        }
    }

    // Drop watcher to stop watching
    drop(watcher);
}
