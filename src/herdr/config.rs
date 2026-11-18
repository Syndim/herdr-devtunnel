use std::path::PathBuf;

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

use super::validation::{parse_machine, parse_session, parse_tunnel};

fn default_max_bridge_connections() -> u16 {
    128
}

fn legacy_max_bridge_connections() -> u16 {
    32
}

fn deserialize_max_bridge_connections<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u16, D::Error> {
    let value = u16::deserialize(deserializer)?;
    if !(1..=4096).contains(&value) {
        return Err(serde::de::Error::custom(
            "bridge connection limit must be between 1 and 4096",
        ));
    }
    Ok(value)
}
#[derive(Clone, Copy, ValueEnum, PartialEq)]
pub(crate) enum Role {
    Client,
    Server,
    Both,
}

#[derive(Args, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Tools {
    #[arg(long, default_value = "hc")]
    pub(crate) hc: PathBuf,
    #[arg(long, default_value = "devtunnel")]
    pub(crate) devtunnel: PathBuf,
}

#[derive(Args)]
pub(crate) struct DoctorArgs {
    #[arg(long, value_enum, default_value = "both")]
    pub(crate) role: Role,
    #[command(flatten)]
    pub(crate) tools: Tools,
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Args)]
pub(crate) struct SetupArgs {
    #[command(flatten)]
    pub(crate) doctor: DoctorArgs,
    /// Explicitly start the official interactive Dev Tunnels login.
    #[arg(long, conflicts_with = "json")]
    pub(crate) login: bool,
}

#[derive(Args, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Connection {
    /// Tunnel name or NAME.CLUSTER (server creation honors the relay suffix).
    #[arg(long, value_parser = parse_tunnel)]
    pub(crate) tunnel: String,
    #[arg(long, default_value_t = 43821, value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) port: u16,
    #[arg(long, default_value = "default", value_parser = parse_session, help = "Server hc session (the remote session for client registration)")]
    pub(crate) session: String,
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=600))]
    pub(crate) timeout: u64,
    #[command(flatten)]
    pub(crate) tools: Tools,
}

#[derive(Args, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ServerArgs {
    #[command(flatten)]
    pub(crate) connection: Connection,
    /// Managed server profile.
    #[arg(long, default_value = "default", value_parser = parse_machine)]
    pub(crate) name: String,
    /// Keep the guarded server attached to this terminal instead.
    #[arg(long)]
    pub(crate) foreground: bool,
    /// Delay in seconds between tunnel host restarts.
    #[arg(long, default_value_t = 2)]
    pub(crate) cooldown: u64,
    /// Maximum tunnel host restarts before exiting (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    pub(crate) max_restarts: u32,
    /// Maximum concurrent bridge connections, including handshakes.
    #[arg(long, default_value_t = default_max_bridge_connections(), value_parser = clap::value_parser!(u16).range(1..=4096))]
    #[serde(
        default = "legacy_max_bridge_connections",
        deserialize_with = "deserialize_max_bridge_connections"
    )]
    pub(crate) max_bridge_connections: u16,
    /// Additional case-insensitive failure regex (repeatable).
    #[arg(long = "pattern", value_name = "REGEX")]
    pub(crate) patterns: Vec<String>,
}

#[derive(Args)]
pub(crate) struct ClientArgs {
    #[command(flatten)]
    pub(crate) connection: Connection,
    #[arg(long, value_parser = parse_machine)]
    pub(crate) name: String,
    #[arg(long, value_parser = parse_session, help = "Local hc session for registration; sets HC_SESSION and removes inherited HC_SOCKET for hc machine add. If omitted, inherits the environment")]
    pub(crate) local_session: Option<String>,
}

#[derive(Args)]
pub(crate) struct ConnectArgs {
    #[command(flatten)]
    pub(crate) connection: Connection,
    /// hc-generated bootstrap: validated, never executed.
    pub(crate) bootstrap: String,
}

#[derive(Args)]
pub(crate) struct TunnelArgs {
    #[arg(value_parser = ["host", "connect"])]
    pub(crate) mode: String,
    #[arg(long, value_parser = parse_tunnel)]
    pub(crate) tunnel: String,
    #[arg(long)]
    pub(crate) devtunnel: PathBuf,
}
