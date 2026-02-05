use anyhow::Result;
use pcpaneld_core::ipc::{IpcRequest, IpcResponse};

use super::{check_response, send_request, truncate};

pub async fn run() -> Result<()> {
    let resp = check_response(send_request(IpcRequest::ListDevices).await?)?;
    match resp {
        IpcResponse::Devices { devices } => {
            if devices.is_empty() {
                println!("No audio devices found.");
            } else {
                println!(
                    "{:<7} {:<6} {:<40} {:<8} {:<6}",
                    "TYPE", "INDEX", "DESCRIPTION", "VOLUME", "MUTED"
                );
                for dev in &devices {
                    let type_str = match dev.device_type {
                        pcpaneld_core::audio::DeviceType::Output => "output",
                        pcpaneld_core::audio::DeviceType::Input => "input",
                    };
                    println!(
                        "{:<7} {:<6} {:<40} {:<8.0}% {:<6}",
                        type_str,
                        dev.index,
                        truncate(&dev.description, 39),
                        dev.volume.get() * 100.0,
                        if dev.muted { "yes" } else { "no" },
                    );
                }
            }
        }
        _ => anyhow::bail!("unexpected response"),
    }
    Ok(())
}
