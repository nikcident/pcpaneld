mod apps;
mod assign;
mod config;
mod devices;
mod info;

use anyhow::{Context, Result};
use pcpaneld_core::ipc::{self, IpcRequest, IpcResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::Commands;

/// Run a CLI command by sending an IPC request to the daemon.
pub async fn run(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::Info => info::run().await,
        Commands::Apps => apps::run().await,
        Commands::Devices => devices::run().await,
        Commands::Assign {
            control,
            action,
            value,
            binary,
            name,
            flatpak_id,
        } => assign::run_assign(control, action, value, binary, name, flatpak_id).await,
        Commands::Unassign { control } => assign::run_unassign(control).await,
        Commands::Config { command } => config::run(command).await,
        Commands::Daemon { .. } => unreachable!("daemon command handled in main"),
    }
}

fn check_response(resp: IpcResponse) -> Result<IpcResponse> {
    match resp {
        IpcResponse::Error { message } => anyhow::bail!("{message}"),
        other => Ok(other),
    }
}

async fn send_request(request: IpcRequest) -> Result<IpcResponse> {
    let socket_path = ipc::default_socket_path();

    let mut stream = UnixStream::connect(&socket_path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::ConnectionRefused
            || e.kind() == std::io::ErrorKind::NotFound
        {
            anyhow::anyhow!(
                "Could not connect to pcpaneld daemon.\n\
                 Is the service running? Try: systemctl --user start pcpaneld.service"
            )
        } else {
            anyhow::anyhow!("failed to connect to daemon: {e}")
        }
    })?;

    // Send request
    let encoded = ipc::encode_request(&request)?;
    stream.write_all(&encoded).await?;
    stream.flush().await?;

    // Read response
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = ipc::read_length_prefix(&len_buf)?;

    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;

    let response: IpcResponse =
        serde_json::from_slice(&payload).context("failed to parse daemon response")?;

    Ok(response)
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..s.floor_char_boundary(max_len)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii_within_limit() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_ascii_at_limit() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_ascii_over_limit() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_multibyte_backs_up_to_char_boundary() {
        // 'ü' is 2 bytes (0xC3 0xBC). "Müsik" = [M, ü(2), s, i, k] = 6 bytes
        // Truncating at 2 would split inside 'ü', should back up to 1
        assert_eq!(truncate("Müsik", 2), "M");
    }

    #[test]
    fn truncate_multibyte_exact_boundary() {
        // Truncating at 3 lands right after 'ü' (byte 3 is 's')
        assert_eq!(truncate("Müsik", 3), "Mü");
    }

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn truncate_zero_max() {
        assert_eq!(truncate("hello", 0), "");
    }
}
