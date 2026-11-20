use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::herdr::config::{
    ClientArgs, ConnectArgs, DoctorArgs, ServerStartArgs, SetupArgs, TunnelArgs,
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
    /// Manage guarded server bridges.
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    /// Register or reconnect client-side remote machines.
    Client {
        #[command(subcommand)]
        command: ClientCommands,
    },
    #[command(name = "_tunnel", hide = true)]
    Tunnel(TunnelArgs),
    #[command(hide = true)]
    ServerWorker(crate::background::WorkerArgs),
}

#[derive(Subcommand)]
pub(crate) enum ClientCommands {
    /// Register a remote machine with the local hc core, without SSH.
    #[command(alias = "add")]
    Connect(ClientArgs),
    /// Reconnect managed servers and registered private-tunnel machines.
    #[command(alias = "r")]
    Reconnect(crate::herdr::reconnect::Arguments),
    /// Internal transport used by hc machine registrations.
    #[command(hide = true)]
    Transport(ConnectArgs),
}

#[derive(Subcommand)]
pub(crate) enum ServerCommands {
    /// Create or reuse a private tunnel and start a guarded bridge.
    Start(ServerStartArgs),
    /// Stop only the named managed bridge and its tunnel host, not hc/agents.
    Stop(crate::background::SessionArgs),
    /// Show whether a managed server is running and its tunnel is ready.
    Status(crate::background::SessionArgs),
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
                Self::Server {
                    command: ServerCommands::Start(args),
                } => {
                    let args = args.resolve()?;
                    if args.foreground {
                        let reporter = crate::background::Reporter::foreground(args.name.clone());
                        crate::herdr::server::serve(args, &reporter).await
                    } else {
                        crate::background::start(args)
                    }
                }
                Self::Server {
                    command: ServerCommands::Stop(args),
                } => crate::background::stop(args),
                Self::Server {
                    command: ServerCommands::Status(args),
                } => crate::background::status(args),
                Self::Client {
                    command: ClientCommands::Connect(args),
                } => crate::herdr::client::add(args).await,
                Self::Client {
                    command: ClientCommands::Reconnect(args),
                } => crate::herdr::reconnect::run(args).await,
                Self::Client {
                    command: ClientCommands::Transport(args),
                } => crate::herdr::server::connect(args).await,
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
