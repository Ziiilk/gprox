mod api;
mod backend;
mod config;
mod process;
mod service;
mod update;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::{net::IpAddr, path::PathBuf};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// State directory (default: ~/.gprox)
    #[arg(long, global = true, env = "GPROX_HOME")]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run in the foreground; Ctrl+C stops the proxy
    Start(StartArgs),
    /// Stop the active foreground or background proxy
    Stop,
    /// Manage a detached background process (no auto-start registration)
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Show running state and connection URL
    Status,
    /// Print the proxy API key (creates credentials on first use)
    Key,
    /// Update this executable from the latest stable GitHub release
    Update {
        /// Check for a new release without downloading or changing anything
        #[arg(long)]
        check: bool,
    },
    /// Show package version information
    Version {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ServiceCommand {
    Start(StartArgs),
    Stop,
    Status,
}

#[derive(Args, Clone, Default)]
struct StartArgs {
    /// Listening IP; use 0.0.0.0 to allow LAN connections
    #[arg(long)]
    host: Option<IpAddr>,
    #[arg(long)]
    port: Option<u16>,
    /// Absolute path to a native Codex executable
    #[arg(long)]
    codex: Option<PathBuf>,
    /// Maximum total duration per request, in seconds
    #[arg(long)]
    timeout: Option<u64>,
    /// Maximum simultaneous Codex requests
    #[arg(long)]
    max_concurrency: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Update { check } = cli.command {
        return update::execute(cli.home, check).await;
    }
    if let Command::Version { json } = cli.command {
        update::print_version(json);
        return Ok(());
    }
    let home = config::home(cli.home)?;
    match cli.command {
        Command::Start(args) => service::run(home, args).await,
        Command::Stop
        | Command::Service {
            command: ServiceCommand::Stop,
        } => service::stop(&home).await,
        Command::Status
        | Command::Service {
            command: ServiceCommand::Status,
        } => service::status(&home).await,
        Command::Service {
            command: ServiceCommand::Start(args),
        } => service::start_background(&home, args).await,
        Command::Key => {
            let _lock = config::lock(&home, "settings.lock")?;
            println!("{}", config::Settings::load(&home)?.api_key);
            Ok(())
        }
        Command::Update { .. } | Command::Version { .. } => unreachable!(),
    }
}
