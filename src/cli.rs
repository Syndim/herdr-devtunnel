use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::herdr::config::{
    ClientArgs, ConnectArgs, DoctorArgs, ServerArgs, SetupArgs, TunnelArgs,
};

#[derive(Parser)]
#[command(
    name = "hd",
    version,
    about = "Connect Herdr cores through private Dev Tunnels"
)]
pub(crate) struct Arguments {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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
    Status(crate::background::SessionArgs),
    /// Stop only the named managed bridge and its tunnel host, not hc/agents.
    Stop(crate::background::SessionArgs),
    /// Reconnect managed servers and registered private-tunnel machines.
    #[command(alias = "r")]
    Reconnect(crate::herdr::reconnect::Arguments),
    /// Register a remote machine with the local hc core, without SSH.
    #[command(alias = "add")]
    Client(ClientArgs),
    #[command(hide = true)]
    Connect(ConnectArgs),
    #[command(name = "_tunnel", hide = true)]
    Tunnel(TunnelArgs),
    #[command(hide = true)]
    ServerWorker(crate::background::WorkerArgs),
}

impl Commands {
    fn run(self) -> Result<()> {
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
                    crate::herdr::server::serve(args, &crate::background::Reporter::foreground())
                        .await
                }
                Self::Server(args) => crate::background::start(args),
                Self::Status(args) => crate::background::status(args),
                Self::Stop(args) => crate::background::stop(args),
                Self::Reconnect(args) => crate::herdr::reconnect::run(args).await,
                Self::Client(args) => crate::herdr::client::add(args).await,
                Self::Connect(args) => crate::herdr::server::connect(args).await,
                Self::Tunnel(args) => crate::devtunnel::process::tunnel_worker(args).await,
                Self::ServerWorker(args) => crate::background::worker(args).await,
            }
        });
        runtime.shutdown_background();
        result
    }
}

pub(crate) fn run() -> Result<()> {
    Arguments::parse().command.run()
}
