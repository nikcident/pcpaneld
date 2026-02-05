use anyhow::Result;
use pcpaneld_core::ipc::{IpcRequest, IpcResponse};

use super::{check_response, send_request, truncate};

pub async fn run() -> Result<()> {
    let resp = check_response(send_request(IpcRequest::ListApps).await?)?;
    match resp {
        IpcResponse::Apps { apps } => {
            if apps.is_empty() {
                println!("No audio apps running.");
            } else {
                println!(
                    "{:<6} {:<30} {:<20} {:<30} {:<8} {:<8} {:<6}",
                    "INDEX", "NAME", "BINARY", "FLATPAK ID", "PID", "VOLUME", "MUTED"
                );
                for app in &apps {
                    let pid_str = app
                        .pid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{:<6} {:<30} {:<20} {:<30} {:<8} {:<8.0}% {:<6}",
                        app.index,
                        truncate(&app.name, 29),
                        truncate(app.binary.as_deref().unwrap_or("-"), 19),
                        truncate(app.flatpak_id.as_deref().unwrap_or("-"), 29),
                        pid_str,
                        app.volume.get() * 100.0,
                        if app.muted { "yes" } else { "no" },
                    );
                }
            }
        }
        _ => anyhow::bail!("unexpected response"),
    }
    Ok(())
}
