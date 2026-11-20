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

const MAX_BRIDGE_WRITE_TIMEOUT: u64 = 86_400;

fn default_bridge_write_timeout() -> u64 {
    120
}

fn deserialize_bridge_write_timeout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    let value = u64::deserialize(deserializer)?;
    if value > MAX_BRIDGE_WRITE_TIMEOUT {
        return Err(serde::de::Error::custom(
            "bridge write timeout must be at most 86400 seconds",
        ));
    }
    Ok(value)
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
    #[arg(long, default_value_t = default_bridge_write_timeout(), value_parser = clap::value_parser!(u64).range(0..=MAX_BRIDGE_WRITE_TIMEOUT), help = "Seconds without write progress in either bridge direction (0 = disabled)")]
    #[serde(default, deserialize_with = "deserialize_bridge_write_timeout")]
    pub(crate) bridge_write_timeout: u64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestServer {
        #[command(flatten)]
        server: ServerArgs,
    }

    #[test]
    fn bridge_write_timeout_cli_and_legacy_config() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["test", "--tunnel", "box"];
            args.extend_from_slice(extra);
            TestServer::try_parse_from(args)
        };
        let server = parse(&[]).unwrap().server;
        assert_eq!(server.bridge_write_timeout, 120);
        assert_eq!(
            parse(&["--bridge-write-timeout", "0"])
                .unwrap()
                .server
                .bridge_write_timeout,
            0
        );
        assert_eq!(
            parse(&["--bridge-write-timeout", "7"])
                .unwrap()
                .server
                .bridge_write_timeout,
            7
        );
        assert!(parse(&["--bridge-write-timeout", "-1"]).is_err());
        assert!(parse(&["--bridge-write-timeout", "invalid"]).is_err());
        assert_eq!(
            parse(&["--bridge-write-timeout", "86400"])
                .unwrap()
                .server
                .bridge_write_timeout,
            MAX_BRIDGE_WRITE_TIMEOUT
        );
        assert!(parse(&["--bridge-write-timeout", "86401"]).is_err());

        let mut config = serde_json::to_value(server).unwrap();
        assert_eq!(config["bridge_write_timeout"], 120);
        config
            .as_object_mut()
            .unwrap()
            .remove("bridge_write_timeout");
        let restored: ServerArgs = serde_json::from_value(config.clone()).unwrap();
        assert_eq!(restored.bridge_write_timeout, 0);
        config["bridge_write_timeout"] = (MAX_BRIDGE_WRITE_TIMEOUT + 1).into();
        assert!(serde_json::from_value::<ServerArgs>(config.clone()).is_err());
        config["bridge_write_timeout"] = MAX_BRIDGE_WRITE_TIMEOUT.into();
        assert_eq!(
            serde_json::from_value::<ServerArgs>(config.clone())
                .unwrap()
                .bridge_write_timeout,
            MAX_BRIDGE_WRITE_TIMEOUT
        );
        config["bridge_write_timeout"] = 0.into();
        let restored: ServerArgs = serde_json::from_value(config).unwrap();
        assert_eq!(restored.bridge_write_timeout, 0);
    }
}
