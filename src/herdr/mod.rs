mod agent;
pub(crate) mod background;
mod client;
mod config;
mod diagnostics;
mod policy;
mod process;
mod protocol;
mod reconnect;
mod server_runtime;
pub(crate) mod terminal_status;
#[cfg(test)]
mod tests;
mod tunnel;

use anyhow::Result;
use clap::{Args, Subcommand};

pub(super) use config::{ClientArgs, ConnectArgs, DoctorArgs, ServerArgs, SetupArgs, TunnelArgs};

#[derive(Args)]
#[command(about = "Connect Herdr cores through owner-only private Dev Tunnels (no SSH)")]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Install agent integrations and report lifecycle status to Herdr.
    Agent(agent::Arguments),
    /// Legacy Copilot hook entry point; use `agent status` instead.
    #[command(hide = true)]
    CopilotStatus(copilot_status::Arguments),
    /// Check prerequisites and print server/client setup instructions.
    Setup(SetupArgs),
    /// Check hc capabilities, devtunnel login, and the local shell requirement.
    Doctor(DoctorArgs),
    /// Create or reuse a private tunnel and start a guarded background bridge.
    #[command(alias = "serve")]
    Server(ServerArgs),
    /// Show whether a managed server is running and its tunnel is ready.
    Status(background::SessionArgs),
    /// Stop only the named managed bridge and its tunnel host, not hc/agents.
    Stop(background::SessionArgs),
    /// Reconnect managed servers and registered private-tunnel machines.
    #[command(alias = "r")]
    Reconnect(reconnect::Arguments),
    /// Register a remote machine with the local hc core, without SSH.
    #[command(alias = "add")]
    Client(ClientArgs),
    #[command(hide = true)]
    Connect(ConnectArgs),
    #[command(name = "_tunnel", hide = true)]
    Tunnel(TunnelArgs),
    #[command(hide = true)]
    ServerWorker(background::WorkerArgs),
}

mod copilot_status;

impl Commands {
    pub(crate) fn run(self) -> Result<()> {
        let command = match self {
            Self::Agent(args) => return args.run(),
            Self::CopilotStatus(args) => return args.run(),
            command => command,
        };
        let runtime = tokio::runtime::Runtime::new()?;
        let result = runtime.block_on(async {
            match command {
                Self::Agent(_) | Self::CopilotStatus(_) => unreachable!(),
                Self::Doctor(args) => diagnostics::doctor(args).await,
                Self::Setup(args) => diagnostics::setup(args).await,
                Self::Server(args) if args.foreground => {
                    server_runtime::serve(args, &background::Reporter::foreground()).await
                }
                Self::Server(args) => background::start(args),
                Self::Status(args) => background::status(args),
                Self::Stop(args) => background::stop(args),
                Self::Reconnect(args) => reconnect::run(args).await,
                Self::Client(args) => client::add(args).await,
                Self::Connect(args) => server_runtime::connect(args).await,
                Self::Tunnel(args) => tunnel::tunnel_worker(args).await,
                Self::ServerWorker(args) => background::worker(args).await,
            }
        });
        runtime.shutdown_background();
        result
    }
}
