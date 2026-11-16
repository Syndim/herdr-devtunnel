use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
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

pub(crate) fn identifier(value: &str, max: usize, punctuation: &str) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || punctuation.contains(c))
}

pub(crate) fn parse_session(value: &str) -> std::result::Result<String, String> {
    if identifier(value, 64, "_.-") {
        Ok(value.into())
    } else {
        Err("Use a session of 1-64 ASCII letters/digits, underscores, dots or hyphens".into())
    }
}

pub(crate) fn parse_machine(value: &str) -> std::result::Result<String, String> {
    if identifier(value, 64, "_-") {
        Ok(value.into())
    } else {
        Err("Use a machine name of 1-64 ASCII letters/digits, underscores or hyphens".into())
    }
}

pub(crate) fn parse_tunnel(value: &str) -> std::result::Result<String, String> {
    if value.len() >= 2 && identifier(value, 151, ".-") {
        Ok(value.into())
    } else {
        Err("Use an explicit tunnel name or NAME.CLUSTER identifier".into())
    }
}

pub(crate) fn executable(path: &PathBuf) -> Result<PathBuf> {
    which::which(path).with_context(|| format!("Executable not found: {}", path.display()))
}

#[cfg(any(not(windows), test))]
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(windows, test))]
pub(crate) fn windows_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                quoted.push('"');
                slashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', slashes));
                quoted.push(ch);
                slashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn via_path(path: PathBuf) -> Result<String> {
    path.into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("hc --via requires UTF-8 executable paths"))
}

#[cfg(not(windows))]
pub(crate) fn client_shell() -> Result<PathBuf> {
    which::which("sh").context("hc --via needs sh on PATH")
}
