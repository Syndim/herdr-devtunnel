mod api;
mod registration;

use std::path::PathBuf;

use anyhow::{Result, ensure};
use clap::Args;

use crate::background;
use crate::herdr::config::Role;
use crate::herdr::validation::parse_session;

#[derive(Args)]
pub struct Arguments {
    /// Reconnect all running server profiles and registered Herdr tunnel machines.
    #[arg(
        long,
        required = true,
        long_help = "Reconnect running servers and registered Herdr tunnel machines. Client registrations must use an absolute executable path resolving to this executable, including through symlinks. Replacing the binary at the same path preserves recognition; a different path, even a copy, does not."
    )]
    all: bool,
    /// Limit reconnection to local servers or remote client connections.
    #[arg(long, value_enum, default_value = "both")]
    pub(super) role: Role,
    /// Local hc session containing the remote machine registrations.
    #[arg(long, visible_alias = "local-session", default_value = "default", value_parser = parse_session)]
    pub(super) session: String,
    #[arg(long, default_value = "hc")]
    pub(super) hc: PathBuf,
    /// Maximum seconds for each local hc API call.
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=600))]
    pub(super) timeout: u64,
}

#[derive(Args)]
pub struct ReRegisterArguments {
    /// Local hc session containing the remote machine registrations.
    #[arg(long, visible_alias = "local-session", default_value = "default", value_parser = parse_session)]
    session: String,
    #[arg(long, default_value = "hc")]
    hc: PathBuf,
    /// Maximum seconds for each local hc API call.
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(1..=600))]
    timeout: u64,
}

pub(crate) async fn reregister(args: ReRegisterArguments) -> Result<()> {
    let args = Arguments {
        all: true,
        role: Role::Client,
        session: args.session,
        hc: args.hc,
        timeout: args.timeout,
    };
    registration::reregister_clients(&args).await
}

pub(crate) async fn run(args: Arguments) -> Result<()> {
    let mut failures = Vec::new();
    if args.role != Role::Client
        && let Err(error) = background::reconnect_all()
    {
        eprintln!("[herdr] Server reconnect failed: {error:#}");
        failures.push(format!("servers: {error:#}"));
    }
    if args.role != Role::Server
        && let Err(error) = registration::reconnect_clients(&args).await
    {
        eprintln!("[herdr] Client reconnect failed: {error:#}");
        failures.push(format!("clients: {error:#}"));
    }
    ensure!(
        failures.is_empty(),
        "Herdr reconnect incomplete:\n{}",
        failures.join("\n")
    );
    Ok(())
}
