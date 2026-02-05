use anyhow::{Context, Result};
use pcpaneld_core::control::{
    AppMatcher, AudioTarget, ButtonAction, ControlId, DialAction, MediaCommand,
};
use pcpaneld_core::ipc::IpcRequest;

use super::{check_response, send_request};

pub async fn run_assign(
    control: String,
    action: String,
    value: String,
    binary: Option<String>,
    name: Option<String>,
    flatpak_id: Option<String>,
) -> Result<()> {
    let control_id = ControlId::from_config_key(&control)
        .with_context(|| format!("invalid control name: {control}"))?;

    let has_audio_flags = binary.is_some() || name.is_some() || flatpak_id.is_some();

    let request = match action.as_str() {
        "volume" => {
            let audio_target = parse_target(&value, binary, name, flatpak_id)?;
            IpcRequest::AssignDial {
                control: control_id,
                action: DialAction::Volume {
                    target: audio_target,
                },
            }
        }
        "mute" => {
            let audio_target = parse_target(&value, binary, name, flatpak_id)?;
            IpcRequest::AssignButton {
                control: control_id,
                action: ButtonAction::Mute {
                    target: audio_target,
                },
            }
        }
        "media" => {
            if has_audio_flags {
                anyhow::bail!(
                    "--binary, --name, and --flatpak-id are only valid for volume/mute actions"
                );
            }
            let command = parse_media_command(&value)?;
            IpcRequest::AssignButton {
                control: control_id,
                action: ButtonAction::Media { command },
            }
        }
        "exec" => {
            if has_audio_flags {
                anyhow::bail!(
                    "--binary, --name, and --flatpak-id are only valid for volume/mute actions"
                );
            }
            IpcRequest::AssignButton {
                control: control_id,
                action: ButtonAction::Exec {
                    command: value.clone(),
                },
            }
        }
        _ => anyhow::bail!(
            "unknown action: {action} (expected 'volume', 'mute', 'media', or 'exec')"
        ),
    };

    check_response(send_request(request).await?)?;
    println!("Assigned {control} {action} -> {value}");
    Ok(())
}

pub async fn run_unassign(control: String) -> Result<()> {
    let control_id = ControlId::from_config_key(&control)
        .with_context(|| format!("invalid control name: {control}"))?;

    check_response(
        send_request(IpcRequest::Unassign {
            control: control_id,
        })
        .await?,
    )?;
    println!("Unassigned {control}");
    Ok(())
}

fn parse_target(
    target: &str,
    binary: Option<String>,
    name: Option<String>,
    flatpak_id: Option<String>,
) -> Result<AudioTarget> {
    match target {
        "default-output" => Ok(AudioTarget::DefaultOutput),
        "default-input" => Ok(AudioTarget::DefaultInput),
        // Support old names for backwards compatibility
        "default-sink" => Ok(AudioTarget::DefaultOutput),
        "default-source" => Ok(AudioTarget::DefaultInput),
        "app" => {
            let matcher = AppMatcher {
                binary,
                name,
                flatpak_id,
            };
            if !matcher.is_valid() {
                anyhow::bail!(
                    "app target requires at least one of --binary, --name, or --flatpak-id"
                );
            }
            Ok(AudioTarget::App { matcher })
        }
        "focused" => Ok(AudioTarget::FocusedApp),
        _ => anyhow::bail!(
            "unknown target: {target} (expected 'default-output', 'default-input', 'app', or 'focused')"
        ),
    }
}

fn parse_media_command(value: &str) -> Result<MediaCommand> {
    match value {
        "play_pause" => Ok(MediaCommand::PlayPause),
        "play" => Ok(MediaCommand::Play),
        "pause" => Ok(MediaCommand::Pause),
        "next" => Ok(MediaCommand::Next),
        "previous" => Ok(MediaCommand::Previous),
        "stop" => Ok(MediaCommand::Stop),
        _ => anyhow::bail!(
            "unknown media command: {value} (expected 'play_pause', 'play', 'pause', 'next', 'previous', or 'stop')"
        ),
    }
}
