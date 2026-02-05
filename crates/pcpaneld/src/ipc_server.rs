use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pcpaneld_core::ipc::{self, IpcRequest, IpcResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::engine::IpcMessage;

/// Check for and clean up a stale socket file.
///
/// If a socket file exists, try connecting. If connection succeeds, another
/// daemon is already running. If it fails, the socket is stale (from a crash)
/// and can be removed.
pub async fn cleanup_stale_socket(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    match UnixStream::connect(path).await {
        Ok(_) => {
            anyhow::bail!(
                "another pcpaneld instance is already running (socket {} is active)",
                path.display()
            );
        }
        Err(_) => {
            info!("removing stale socket file: {}", path.display());
            tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("failed to remove stale socket {}", path.display()))?;
        }
    }

    Ok(())
}

/// Run the IPC server on a Unix socket.
pub async fn run(
    socket_path: PathBuf,
    request_tx: mpsc::Sender<IpcMessage>,
    cancel: CancellationToken,
) -> Result<()> {
    // Set restrictive umask before binding.
    // Safety: libc::umask() is process-global state, but this runs at startup
    // before any file-creating threads are active, so no race is possible.
    let old_umask = unsafe { libc::umask(0o077) };

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind IPC socket at {}", socket_path.display()))?;

    // Restore umask
    unsafe { libc::umask(old_umask) };

    info!("IPC server listening on {}", socket_path.display());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let tx = request_tx.clone();
                        let client_cancel = cancel.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, tx, client_cancel).await {
                                debug!("IPC client error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        warn!("failed to accept IPC connection: {e}");
                    }
                }
            }
        }
    }

    // Clean up socket on exit
    let _ = tokio::fs::remove_file(&socket_path).await;
    info!("IPC server stopped");
    Ok(())
}

async fn handle_client(
    mut stream: UnixStream,
    request_tx: mpsc::Sender<IpcMessage>,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                return Ok(());
            }
            result = read_request(&mut stream) => {
                match result {
                    Ok(Some(request)) => {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let msg = IpcMessage { request, reply_tx };

                        if request_tx.send(msg).await.is_err() {
                            return Ok(());
                        }

                        match reply_rx.await {
                            Ok(response) => {
                                write_response(&mut stream, &response).await?;
                            }
                            Err(_) => {
                                let resp = IpcResponse::Error {
                                    message: "daemon shutting down".into(),
                                };
                                write_response(&mut stream, &resp).await?;
                                return Ok(());
                            }
                        }
                    }
                    Ok(None) => {
                        return Ok(());
                    }
                    Err(e) => {
                        debug!("IPC read error: {e}");
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn read_request(stream: &mut UnixStream) -> Result<Option<IpcRequest>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    }

    let len = ipc::read_length_prefix(&len_buf)?;

    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;

    let request: IpcRequest =
        serde_json::from_slice(&payload).with_context(|| "failed to parse IPC request")?;

    Ok(Some(request))
}

async fn write_response(stream: &mut UnixStream, response: &IpcResponse) -> Result<()> {
    let encoded = ipc::encode_response(response)?;
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    Ok(())
}
