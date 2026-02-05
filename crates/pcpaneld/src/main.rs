mod cli;
mod config_watcher;
mod daemon;
mod engine;
mod hid;
mod hid_thread;
mod ipc_server;
mod kwin;
mod mpris;
mod pulse;
mod signal;
mod tray;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pcpaneld", about = "PCPanel Pro daemon and control tool")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the daemon (for systemd or manual start)
    Daemon {
        /// Log level (trace, debug, info, warn, error)
        #[arg(long, env = "PCPANELD_LOG_LEVEL", default_value = "info")]
        log_level: String,
    },
    /// Show device, audio, and mapping info
    Info,
    /// List running audio applications
    Apps,
    /// List audio devices (outputs and inputs)
    Devices,
    /// Assign an action to a control
    Assign {
        /// Control name (knob1-knob5, slider1-slider4)
        control: String,
        /// Action type (volume, mute, media, exec)
        action: String,
        /// Target or value
        value: String,
        /// Match by binary name (volume/mute only)
        #[arg(long)]
        binary: Option<String>,
        /// Match by application name (volume/mute only)
        #[arg(long)]
        name: Option<String>,
        /// Match by Flatpak ID (volume/mute only)
        #[arg(long)]
        flatpak_id: Option<String>,
    },
    /// Remove a control assignment
    Unassign {
        /// Control name (knob1-knob5, slider1-slider4)
        control: String,
    },
    /// Configuration commands
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Print current config as TOML
    Show,
    /// Reload config from disk
    Reload,
    /// Print config directory path
    Dir,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        None => {
            use clap::CommandFactory;
            let _ = Cli::command().print_help();
            println!();
            std::process::exit(0);
        }
        // Daemon builds its own multi-thread runtime (needs spawn_blocking for
        // std::thread joins). CLI commands only need a single-threaded runtime
        // for one IPC round-trip.
        Some(Commands::Daemon { log_level }) => daemon::run(&log_level),
        Some(cmd) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime")
            .block_on(cli::run(cmd)),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
