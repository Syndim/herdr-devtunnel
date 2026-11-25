use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::background::StartDefaults;

use super::validation::{parse_machine, parse_session, parse_tunnel};

const DEFAULT_PORT: u16 = 43821;

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
    #[arg(long, default_value_t = DEFAULT_PORT, value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) port: u16,
    #[arg(long, default_value = "default", value_parser = parse_session, help = "Server hc session (the remote session for client registration)")]
    pub(crate) session: String,
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=600))]
    pub(crate) timeout: u64,
    #[command(flatten)]
    pub(crate) tools: Tools,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ServerArgs {
    pub(crate) connection: Connection,
    pub(crate) name: String,
    pub(crate) foreground: bool,
    pub(crate) cooldown: u64,
    pub(crate) max_restarts: u32,
    #[serde(
        default = "legacy_max_bridge_connections",
        deserialize_with = "deserialize_max_bridge_connections"
    )]
    pub(crate) max_bridge_connections: u16,
    #[serde(default, deserialize_with = "deserialize_bridge_write_timeout")]
    pub(crate) bridge_write_timeout: u64,
    pub(crate) patterns: Vec<String>,
}

#[derive(Args)]
pub(crate) struct ServerStartArgs {
    #[arg(long, value_parser = parse_tunnel, help = "Tunnel name or NAME.CLUSTER (server creation honors the relay suffix)")]
    tunnel: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    #[arg(long, default_value = "default", value_parser = parse_session, help = "Server hc session (the remote session for client registration)")]
    session: String,
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=600))]
    timeout: u64,
    #[command(flatten)]
    tools: Tools,
    #[arg(long, value_parser = parse_machine, help = "Managed server profile")]
    name: Option<String>,
    #[arg(
        long,
        help = "Keep the guarded server attached to this terminal instead"
    )]
    foreground: bool,
    #[arg(
        long,
        default_value_t = 2,
        help = "Delay in seconds between tunnel host restarts"
    )]
    cooldown: u64,
    #[arg(
        long,
        default_value_t = 0,
        help = "Maximum tunnel host restarts before exiting (0 = unlimited)"
    )]
    max_restarts: u32,
    #[arg(long, default_value_t = default_max_bridge_connections(), value_parser = clap::value_parser!(u16).range(1..=4096), help = "Maximum concurrent bridge connections, including handshakes")]
    max_bridge_connections: u16,
    #[arg(long, default_value_t = default_bridge_write_timeout(), value_parser = clap::value_parser!(u64).range(0..=MAX_BRIDGE_WRITE_TIMEOUT), help = "Seconds without write progress in either bridge direction (0 = disabled)")]
    bridge_write_timeout: u64,
    #[arg(
        long = "pattern",
        value_name = "REGEX",
        help = "Additional case-insensitive failure regex (repeatable)"
    )]
    patterns: Vec<String>,
}

impl ServerStartArgs {
    pub(crate) fn resolve(self) -> Result<ServerArgs> {
        let can_start_without_storage = self.foreground && self.tunnel.is_some();
        self.resolve_with(|| match StartDefaults::load() {
            Err(error) if can_start_without_storage => {
                eprintln!("[herdr] Could not load server start settings: {error:#}");
                Ok(None)
            }
            result => result,
        })
    }

    fn resolve_with(
        self,
        load: impl FnOnce() -> Result<Option<StartDefaults>>,
    ) -> Result<ServerArgs> {
        let defaults = if self.tunnel.is_none() || self.name.is_none() || self.port.is_none() {
            load()?
        } else {
            None
        };
        let (tunnel, name, port) = match defaults {
            Some(defaults) => (Some(defaults.tunnel), defaults.name, defaults.port),
            None => (None, "default".into(), DEFAULT_PORT),
        };
        let tunnel = self.tunnel.or(tunnel).ok_or_else(|| {
            anyhow!("--tunnel is required when no server start defaults are configured")
        })?;
        let tunnel =
            parse_tunnel(&tunnel).map_err(|error| anyhow!("Invalid saved tunnel: {error}"))?;
        let name = parse_machine(&self.name.unwrap_or(name))
            .map_err(|error| anyhow!("Invalid saved profile name: {error}"))?;
        let port = self.port.unwrap_or(port);
        anyhow::ensure!(port != 0, "Invalid saved port: port must be nonzero");
        Ok(ServerArgs {
            connection: Connection {
                tunnel,
                port,
                session: self.session,
                timeout: self.timeout,
                tools: self.tools,
            },
            name,
            foreground: self.foreground,
            cooldown: self.cooldown,
            max_restarts: self.max_restarts,
            max_bridge_connections: self.max_bridge_connections,
            bridge_write_timeout: self.bridge_write_timeout,
            patterns: self.patterns,
        })
    }
}

#[derive(Args)]
pub(crate) struct ClientArgs {
    #[command(flatten)]
    pub(crate) connection: Connection,
    #[arg(long, value_parser = parse_machine)]
    pub(crate) name: String,
    #[arg(long, value_parser = parse_session, help = "Local hc session for registration; sets HC_SESSION and removes inherited HC_SOCKET for hc machine add. If omitted, inherits the environment")]
    pub(crate) local_session: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Executable path saved for hc transport (for example, a mise shim)"
    )]
    pub(crate) hd_path: Option<PathBuf>,
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
        server: ServerStartArgs,
    }

    #[derive(Parser)]
    struct TestClient {
        #[command(flatten)]
        client: ClientArgs,
    }

    fn parse_start(flags: &[&str]) -> Result<ServerStartArgs, clap::Error> {
        TestServer::try_parse_from(["test"].into_iter().chain(flags.iter().copied()))
            .map(|args| args.server)
    }

    #[test]
    fn server_start_resolves_each_default_independently() {
        for (flags, expected) in [
            (&[][..], ("saved-tunnel", "saved-name", 1234)),
            (&["--tunnel", "box"], ("box", "saved-name", 1234)),
            (&["--name", "default"], ("saved-tunnel", "default", 1234)),
            (&["--port", "43821"], ("saved-tunnel", "saved-name", 43821)),
        ] {
            let server = parse_start(flags)
                .unwrap()
                .resolve_with(|| {
                    Ok(Some(StartDefaults {
                        tunnel: "saved-tunnel".into(),
                        name: "saved-name".into(),
                        port: 1234,
                    }))
                })
                .unwrap();
            assert_eq!(
                (
                    server.connection.tunnel.as_str(),
                    server.name.as_str(),
                    server.connection.port
                ),
                expected
            );
        }
    }

    #[test]
    fn server_start_without_saved_defaults() {
        let server = parse_start(&["--tunnel", "box"])
            .unwrap()
            .resolve_with(|| Ok(None))
            .unwrap();
        assert_eq!(server.connection.port, 43821);
        assert_eq!(server.name, "default");
        let result = parse_start(&[]).unwrap().resolve_with(|| Ok(None));
        assert!(matches!(result, Err(error) if error.to_string().contains("--tunnel")));
    }

    #[test]
    fn server_start_only_loads_defaults_when_a_flag_is_omitted() {
        assert!(
            parse_start(&["--tunnel", "box", "--name", "dev", "--port", "1234"])
                .unwrap()
                .resolve_with(|| panic!("defaults are not needed"))
                .is_ok()
        );
        let result = parse_start(&["--tunnel", "box"])
            .unwrap()
            .resolve_with(|| Err(anyhow!("failed to load defaults")));
        assert!(matches!(result, Err(error) if error.to_string() == "failed to load defaults"));
    }

    #[test]
    fn client_still_requires_tunnel_and_name() {
        assert!(TestClient::try_parse_from(["test", "--name", "dev"]).is_err());
        assert!(TestClient::try_parse_from(["test", "--tunnel", "box"]).is_err());
        let args =
            TestClient::try_parse_from(["test", "--tunnel", "box", "--name", "dev"]).unwrap();
        assert_eq!(args.client.connection.port, 43821);
    }

    #[test]
    fn bridge_write_timeout_cli_and_legacy_config() {
        let parse = |flags: &[&str]| {
            let mut args = vec!["--tunnel", "box"];
            args.extend_from_slice(flags);
            parse_start(&args)?.resolve_with(|| Ok(None))
        };
        let server = parse(&[]).unwrap();
        assert_eq!(server.bridge_write_timeout, 120);
        assert_eq!(
            parse(&["--bridge-write-timeout", "0"])
                .unwrap()
                .bridge_write_timeout,
            0
        );
        assert_eq!(
            parse(&["--bridge-write-timeout", "7"])
                .unwrap()
                .bridge_write_timeout,
            7
        );
        assert!(parse(&["--bridge-write-timeout", "-1"]).is_err());
        assert!(parse(&["--bridge-write-timeout", "invalid"]).is_err());
        assert_eq!(
            parse(&["--bridge-write-timeout", "86400"])
                .unwrap()
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
