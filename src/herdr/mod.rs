pub(crate) mod background;
mod client;
pub(crate) mod protocol;
mod reconnect;
mod server;

#[cfg(test)]
mod tests;

use anyhow::Result;
use clap::{Args, Subcommand};

pub(super) use crate::config::{
    ClientArgs, ConnectArgs, DoctorArgs, ServerArgs, SetupArgs, TunnelArgs,
};

#[derive(Args)]
#[command(about = "Connect Herdr cores through owner-only private Dev Tunnels (no SSH)")]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Install agent integrations and report lifecycle status to Herdr.
    Agent(crate::agent::Arguments),
    /// Legacy Copilot hook entry point; use `agent status` instead.
    #[command(hide = true)]
    CopilotStatus(crate::agent::copilot_status::Arguments),
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
                Self::Doctor(args) => crate::diagnostics::doctor(args).await,
                Self::Setup(args) => crate::diagnostics::setup(args).await,
                Self::Server(args) if args.foreground => {
                    server::serve(args, &background::Reporter::foreground()).await
                }
                Self::Server(args) => background::start(args),
                Self::Status(args) => background::status(args),
                Self::Stop(args) => background::stop(args),
                Self::Reconnect(args) => reconnect::run(args).await,
                Self::Client(args) => client::add(args).await,
                Self::Connect(args) => server::connect(args).await,
                Self::Tunnel(args) => crate::devtunnel::tunnel::tunnel_worker(args).await,
                Self::ServerWorker(args) => background::worker(args).await,
            }
        });
        runtime.shutdown_background();
        result
    }
}
