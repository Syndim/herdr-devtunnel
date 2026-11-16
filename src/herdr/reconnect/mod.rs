mod api;
mod transport;

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use serde_json::json;

use super::background;
use crate::config::{Role, parse_session};

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

pub(super) async fn run(args: Arguments) -> Result<()> {
    let mut failures = Vec::new();
    if args.role != Role::Client
        && let Err(error) = background::reconnect_all()
    {
        eprintln!("[herdr] Server reconnect failed: {error:#}");
        failures.push(format!("servers: {error:#}"));
    }
    if args.role != Role::Server
        && let Err(error) = reconnect_clients(&args).await
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

#[derive(Deserialize)]
struct Machine {
    name: String,
    target: String,
    #[serde(default)]
    via: bool,
}

#[derive(Deserialize)]
struct Machines {
    machines: Vec<Machine>,
}

async fn reconnect_clients(args: &Arguments) -> Result<()> {
    let machines: Machines =
        serde_json::from_value(api::call_local(args, "machine.list", json!({})).await?)
            .context("Invalid hc machine list")?;
    let current_exe = std::env::current_exe().context("Cannot identify reconnect executable")?;
    let mut count = 0;
    let mut failures = Vec::new();
    for machine in machines.machines {
        if !machine.via || !transport::is_our_transport(&machine.target, &current_exe) {
            continue;
        }
        count += 1;
        let result = async {
            let result =
                api::call_local(args, "machine.connect", json!({"machine": machine.name})).await?;
            let state = result["state"]
                .as_str()
                .context("hc machine.connect returned no state")?;
            ensure!(
                state == "connected",
                "state={state}; {}",
                result["why"].as_str().unwrap_or("connection is not ready")
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => println!("Machine '{}' is connected.", machine.name),
            Err(error) => {
                eprintln!("[herdr] Machine '{}': {error:#}", machine.name);
                failures.push(format!("{}: {error:#}", machine.name));
            }
        }
    }
    if count == 0 {
        println!("No registered Herdr dev-tunnel machines found.");
    }
    ensure!(
        failures.is_empty(),
        "Failed to reconnect {} machine(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}
