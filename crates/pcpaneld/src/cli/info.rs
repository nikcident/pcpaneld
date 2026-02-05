use anyhow::Result;
use pcpaneld_core::ipc::{DeviceStatus, IpcRequest, IpcResponse, MappingInfo};

use super::{check_response, send_request};

pub async fn run() -> Result<()> {
    let resp = check_response(send_request(IpcRequest::GetStatus).await?)?;
    match resp {
        IpcResponse::Status {
            device,
            pulse_connected,
            mappings,
        } => {
            print_status(&device, pulse_connected, &mappings);
        }
        _ => anyhow::bail!("unexpected response"),
    }
    Ok(())
}

fn print_status(device: &DeviceStatus, pulse_connected: bool, mappings: &[MappingInfo]) {
    println!("Device:");
    if device.connected {
        println!(
            "  Connected (serial: {})",
            device.serial.as_deref().unwrap_or("unknown")
        );
    } else {
        println!("  Disconnected");
    }

    println!(
        "PulseAudio: {}",
        if pulse_connected {
            "connected"
        } else {
            "disconnected"
        }
    );

    if mappings.is_empty() {
        println!("Mappings: none");
    } else {
        println!("Mappings:");
        for m in mappings {
            let dial = m.dial.as_deref().unwrap_or("-");
            let button = m.button.as_deref().unwrap_or("-");
            println!("  {:<10} dial: {:<30} button: {}", m.control, dial, button);
        }
    }
}
