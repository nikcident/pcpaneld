use anyhow::Result;
use pcpaneld_core::config::Config;
use pcpaneld_core::ipc::{IpcRequest, IpcResponse};

use super::{check_response, send_request};
use crate::ConfigCommands;

pub async fn run(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Show => {
            let resp = check_response(send_request(IpcRequest::GetConfig).await?)?;
            match resp {
                IpcResponse::Config { toml } => print!("{toml}"),
                _ => anyhow::bail!("unexpected response"),
            }
        }
        ConfigCommands::Reload => {
            check_response(send_request(IpcRequest::ReloadConfig).await?)?;
            println!("Config reloaded.");
        }
        ConfigCommands::Dir => {
            let dir = Config::default_dir().expect("failed to resolve XDG config directory");
            println!("{}", dir.display());
        }
    }
    Ok(())
}
