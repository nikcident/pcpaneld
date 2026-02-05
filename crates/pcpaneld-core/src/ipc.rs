use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::audio::{DeviceInfo, SinkInfo, SinkInputInfo, SourceInfo};
use crate::control::{ButtonAction, ControlId, DialAction};

/// Maximum IPC message size (1 MB).
pub const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

#[derive(Error, Debug)]
pub enum IpcError {
    #[error("IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("IPC message too large: {size} bytes (max {MAX_MESSAGE_SIZE})")]
    MessageTooLarge { size: u32 },
    #[error("failed to serialize IPC message: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("connection refused: is the daemon running?")]
    ConnectionRefused,
}

/// Requests from pcpaneld CLI to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    GetStatus,
    ListApps,
    ListDevices,
    /// Legacy: list output devices only
    #[serde(alias = "list_sinks")]
    ListOutputs,
    /// Legacy: list input devices only
    #[serde(alias = "list_sources")]
    ListInputs,
    AssignDial {
        control: ControlId,
        action: DialAction,
    },
    AssignButton {
        control: ControlId,
        action: ButtonAction,
    },
    Unassign {
        control: ControlId,
    },
    GetConfig,
    ReloadConfig,
    Shutdown,
}

/// Device connection status reported via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub connected: bool,
    pub serial: Option<String>,
}

/// A single control mapping for status display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingInfo {
    pub control: String,
    pub dial: Option<String>,
    pub button: Option<String>,
}

/// Responses from the daemon to pcpaneld CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok,
    Error {
        message: String,
    },
    Status {
        device: DeviceStatus,
        pulse_connected: bool,
        mappings: Vec<MappingInfo>,
    },
    Apps {
        apps: Vec<SinkInputInfo>,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    /// Legacy: output devices only
    #[serde(alias = "sinks")]
    Outputs {
        #[serde(alias = "sinks")]
        outputs: Vec<SinkInfo>,
    },
    /// Legacy: input devices only
    #[serde(alias = "sources")]
    Inputs {
        #[serde(alias = "sources")]
        inputs: Vec<SourceInfo>,
    },
    Config {
        toml: String,
    },
}

/// Encode a message with a 4-byte little-endian length prefix.
pub fn encode_message(msg: &[u8]) -> Result<Vec<u8>, IpcError> {
    let len = u32::try_from(msg.len()).map_err(|_| IpcError::MessageTooLarge { size: u32::MAX })?;
    if len > MAX_MESSAGE_SIZE {
        return Err(IpcError::MessageTooLarge { size: len });
    }
    let mut buf = Vec::with_capacity(4 + msg.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(msg);
    Ok(buf)
}

/// Encode a request as length-prefixed JSON.
pub fn encode_request(req: &IpcRequest) -> Result<Vec<u8>, IpcError> {
    let json = serde_json::to_vec(req)?;
    encode_message(&json)
}

/// Encode a response as length-prefixed JSON.
pub fn encode_response(resp: &IpcResponse) -> Result<Vec<u8>, IpcError> {
    let json = serde_json::to_vec(resp)?;
    encode_message(&json)
}

/// Read a length prefix from a 4-byte buffer.
/// Returns None if the length exceeds the maximum.
pub fn read_length_prefix(buf: &[u8; 4]) -> Result<u32, IpcError> {
    let len = u32::from_le_bytes(*buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(IpcError::MessageTooLarge { size: len });
    }
    Ok(len)
}

/// Returns the XDG runtime directory, falling back to `/run/user/{uid}`.
#[must_use]
pub fn xdg_runtime_dir() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        let uid = unsafe { libc::getuid() };
        format!("/run/user/{uid}")
    });
    std::path::PathBuf::from(dir)
}

/// Returns the default IPC socket path.
#[must_use]
pub fn default_socket_path() -> std::path::PathBuf {
    xdg_runtime_dir().join("pcpaneld.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Volume;
    use crate::control::{AppMatcher, AudioTarget, MediaCommand};

    #[test]
    fn request_serde_round_trip_all_variants() {
        let requests = vec![
            IpcRequest::GetStatus,
            IpcRequest::ListApps,
            IpcRequest::ListDevices,
            IpcRequest::ListOutputs,
            IpcRequest::ListInputs,
            IpcRequest::AssignDial {
                control: ControlId::Knob(0),
                action: DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                },
            },
            IpcRequest::AssignButton {
                control: ControlId::Knob(0),
                action: ButtonAction::Mute {
                    target: AudioTarget::DefaultOutput,
                },
            },
            IpcRequest::Unassign {
                control: ControlId::Slider(2),
            },
            IpcRequest::GetConfig,
            IpcRequest::ReloadConfig,
            IpcRequest::Shutdown,
            IpcRequest::AssignDial {
                control: ControlId::Knob(2),
                action: DialAction::Volume {
                    target: AudioTarget::App {
                        matcher: AppMatcher {
                            flatpak_id: Some("org.mozilla.firefox".into()),
                            ..Default::default()
                        },
                    },
                },
            },
            IpcRequest::AssignDial {
                control: ControlId::Slider(3),
                action: DialAction::Volume {
                    target: AudioTarget::FocusedApp,
                },
            },
            IpcRequest::AssignButton {
                control: ControlId::Knob(3),
                action: ButtonAction::Media {
                    command: MediaCommand::PlayPause,
                },
            },
            IpcRequest::AssignButton {
                control: ControlId::Knob(4),
                action: ButtonAction::Exec {
                    command: "notify-send 'hello'".into(),
                },
            },
        ];

        for req in &requests {
            let json = serde_json::to_string(req).unwrap();
            let parsed: IpcRequest = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2, "round-trip failed for {req:?}");
        }
    }

    #[test]
    fn response_serde_round_trip_all_variants() {
        let responses = vec![
            IpcResponse::Ok,
            IpcResponse::Error {
                message: "something went wrong".into(),
            },
            IpcResponse::Status {
                device: DeviceStatus {
                    connected: true,
                    serial: Some("ABC123".into()),
                },
                pulse_connected: true,
                mappings: vec![MappingInfo {
                    control: "knob1".into(),
                    dial: Some("volume default-output".into()),
                    button: Some("mute default-output".into()),
                }],
            },
            IpcResponse::Apps {
                apps: vec![SinkInputInfo {
                    index: 42,
                    name: "Firefox".into(),
                    binary: Some("firefox".into()),
                    flatpak_id: Some("org.mozilla.firefox".into()),
                    pid: Some(1234),
                    sink_index: 0,
                    volume: Volume::new(0.75),
                    muted: false,
                    channels: 2,
                }],
            },
            IpcResponse::Outputs {
                outputs: vec![SinkInfo {
                    index: 0,
                    name: "alsa_output.pci".into(),
                    description: "Built-in Audio".into(),
                    volume: Volume::new(1.0),
                    muted: false,
                    channels: 2,
                }],
            },
            IpcResponse::Inputs {
                inputs: vec![SourceInfo {
                    index: 0,
                    name: "alsa_input.pci".into(),
                    description: "Built-in Mic".into(),
                    volume: Volume::new(0.5),
                    muted: true,
                    channels: 1,
                }],
            },
            IpcResponse::Config {
                toml: "[device]\nserial = \"ABC\"".into(),
            },
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp).unwrap();
            let parsed: IpcResponse = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2, "round-trip failed for {resp:?}");
        }
    }

    #[test]
    fn length_prefix_encode_decode() {
        let msg = b"hello world";
        let encoded = encode_message(msg).unwrap();
        assert_eq!(encoded.len(), 4 + msg.len());

        let len_bytes: [u8; 4] = encoded[..4].try_into().unwrap();
        let len = read_length_prefix(&len_bytes).unwrap();
        assert_eq!(len as usize, msg.len());
        assert_eq!(&encoded[4..], msg);
    }

    #[test]
    fn oversized_message_rejected() {
        let len = MAX_MESSAGE_SIZE + 1;
        let bytes = len.to_le_bytes();
        let result = read_length_prefix(&bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            IpcError::MessageTooLarge { size } => assert_eq!(size, len),
            e => panic!("expected MessageTooLarge, got {e:?}"),
        }
    }

    #[test]
    fn max_valid_length_accepted() {
        let bytes = MAX_MESSAGE_SIZE.to_le_bytes();
        let result = read_length_prefix(&bytes);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), MAX_MESSAGE_SIZE);
    }

    #[test]
    fn zero_length_message() {
        let encoded = encode_message(b"").unwrap();
        assert_eq!(encoded.len(), 4);
        let len_bytes: [u8; 4] = encoded[..4].try_into().unwrap();
        assert_eq!(read_length_prefix(&len_bytes).unwrap(), 0);
    }

    #[test]
    fn request_encode_decode_round_trip() {
        let req = IpcRequest::AssignDial {
            control: ControlId::Knob(2),
            action: DialAction::Volume {
                target: AudioTarget::App {
                    matcher: AppMatcher {
                        binary: Some("firefox".into()),
                        name: Some("Firefox".into()),
                        flatpak_id: None,
                    },
                },
            },
        };

        let encoded = encode_request(&req).unwrap();
        let len_bytes: [u8; 4] = encoded[..4].try_into().unwrap();
        let len = read_length_prefix(&len_bytes).unwrap() as usize;
        let payload = &encoded[4..4 + len];
        let decoded: IpcRequest = serde_json::from_slice(payload).unwrap();

        let json1 = serde_json::to_string(&req).unwrap();
        let json2 = serde_json::to_string(&decoded).unwrap();
        assert_eq!(json1, json2);
    }

    #[test]
    fn audio_target_display() {
        assert_eq!(AudioTarget::DefaultOutput.to_string(), "default-output");
        assert_eq!(AudioTarget::DefaultInput.to_string(), "default-input");
        assert_eq!(
            AudioTarget::App {
                matcher: AppMatcher {
                    binary: Some("firefox".into()),
                    ..Default::default()
                }
            }
            .to_string(),
            "app(binary=firefox)"
        );
        assert_eq!(
            AudioTarget::App {
                matcher: AppMatcher {
                    binary: Some("firefox".into()),
                    name: Some("Firefox".into()),
                    flatpak_id: None,
                }
            }
            .to_string(),
            "app(binary=firefox, name=Firefox)"
        );
        assert_eq!(AudioTarget::FocusedApp.to_string(), "focused");
    }

    #[test]
    fn response_serde_round_trip_non_ascii() {
        let resp = IpcResponse::Apps {
            apps: vec![SinkInputInfo {
                index: 1,
                name: "Müsik-Plàyér 音楽".into(),
                binary: Some("müsik".into()),
                flatpak_id: Some("org.example.müsik".into()),
                pid: None,
                sink_index: 0,
                volume: Volume::new(0.5),
                muted: false,
                channels: 2,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: IpcResponse = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn socket_path_is_deterministic() {
        let path = default_socket_path();
        assert_eq!(path.file_name().unwrap(), "pcpaneld.sock");
    }

    #[test]
    fn backwards_compat_old_target_names_deserialize() {
        // Verify old config files with default_sink/default_source still work
        let json = r#"{"type":"default_sink"}"#;
        let target: AudioTarget = serde_json::from_str(json).unwrap();
        assert_eq!(target, AudioTarget::DefaultOutput);

        let json = r#"{"type":"default_source"}"#;
        let target: AudioTarget = serde_json::from_str(json).unwrap();
        assert_eq!(target, AudioTarget::DefaultInput);
    }
}
