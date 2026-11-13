mod background;
mod copilot_status;
mod reconnect;
pub(crate) mod terminal_status;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

const MAGIC: &str = "herdr-devtunnel/1";
const HANDSHAKE_LIMIT: usize = 4096;
const BRIDGE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Args)]
#[command(about = "Connect Herdr cores through owner-only private Dev Tunnels (no SSH)")]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Report a Copilot lifecycle hook to the current Herdr terminal.
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

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Role {
    Client,
    Server,
    Both,
}

#[derive(Args, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tools {
    #[arg(long, default_value = "hc")]
    hc: PathBuf,
    #[arg(long, default_value = "devtunnel")]
    devtunnel: PathBuf,
}

#[derive(Args)]
pub struct DoctorArgs {
    #[arg(long, value_enum, default_value = "both")]
    role: Role,
    #[command(flatten)]
    tools: Tools,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct SetupArgs {
    #[command(flatten)]
    doctor: DoctorArgs,
    /// Explicitly start the official interactive Dev Tunnels login.
    #[arg(long, conflicts_with = "json")]
    login: bool,
}

#[derive(Args, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Connection {
    /// Tunnel name or NAME.CLUSTER (server creation honors the relay suffix).
    #[arg(long, value_parser = parse_tunnel)]
    tunnel: String,
    #[arg(long, default_value_t = 43821, value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
    #[arg(long, default_value = "default", value_parser = parse_session, help = "Server hc session (the remote session for client registration)")]
    session: String,
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=600))]
    timeout: u64,
    #[command(flatten)]
    tools: Tools,
}

#[derive(Args, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerArgs {
    #[command(flatten)]
    connection: Connection,
    /// Managed server profile.
    #[arg(long, default_value = "default", value_parser = parse_machine)]
    name: String,
    /// Keep the guarded server attached to this terminal instead.
    #[arg(long)]
    foreground: bool,
    /// Delay in seconds between tunnel host restarts.
    #[arg(long, default_value_t = 2)]
    cooldown: u64,
    /// Maximum tunnel host restarts before exiting (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    max_restarts: u32,
    /// Additional case-insensitive failure regex (repeatable).
    #[arg(long = "pattern", value_name = "REGEX")]
    patterns: Vec<String>,
}

#[derive(Args)]
pub struct ClientArgs {
    #[command(flatten)]
    connection: Connection,
    #[arg(long, value_parser = parse_machine)]
    name: String,
    #[arg(long, value_parser = parse_session, help = "Local hc session for registration; sets HC_SESSION and removes inherited HC_SOCKET for hc machine add. If omitted, inherits the environment")]
    local_session: Option<String>,
}

#[derive(Args)]
pub struct ConnectArgs {
    #[command(flatten)]
    connection: Connection,
    /// hc-generated bootstrap: validated, never executed.
    bootstrap: String,
}

#[derive(Args)]
pub struct TunnelArgs {
    #[arg(value_parser = ["host", "connect"])]
    mode: String,
    #[arg(long, value_parser = parse_tunnel)]
    tunnel: String,
    #[arg(long)]
    devtunnel: PathBuf,
}

impl Commands {
    pub(crate) fn run(self) -> Result<()> {
        if let Self::CopilotStatus(args) = self {
            return args.run();
        }
        let runtime = tokio::runtime::Runtime::new()?;
        let result = runtime.block_on(async {
            match self {
                Self::CopilotStatus(args) => args.run(),
                Self::Doctor(args) => doctor(args).await,
                Self::Setup(args) => setup(args).await,
                Self::Server(args) if args.foreground => {
                    serve(args, &background::Reporter::foreground()).await
                }
                Self::Server(args) => background::start(args),
                Self::Status(args) => background::status(args),
                Self::Stop(args) => background::stop(args),
                Self::Reconnect(args) => reconnect::run(args).await,
                Self::Client(args) => add(args).await,
                Self::Connect(args) => connect(args).await,
                Self::Tunnel(args) => tunnel_worker(args).await,
                Self::ServerWorker(args) => background::worker(args).await,
            }
        });
        // Tokio's console stdin uses a blocking thread; it must not prevent exit
        // after a disconnected remote peer while the local stdin remains open.
        runtime.shutdown_background();
        result
    }
}

fn identifier(value: &str, max: usize, punctuation: &str) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || punctuation.contains(c))
}

fn parse_session(value: &str) -> std::result::Result<String, String> {
    if identifier(value, 64, "_.-") {
        Ok(value.into())
    } else {
        Err("Use a session of 1-64 ASCII letters/digits, underscores, dots or hyphens".into())
    }
}

fn parse_machine(value: &str) -> std::result::Result<String, String> {
    if identifier(value, 64, "_-") {
        Ok(value.into())
    } else {
        Err("Use a machine name of 1-64 ASCII letters/digits, underscores or hyphens".into())
    }
}

fn parse_tunnel(value: &str) -> std::result::Result<String, String> {
    if value.len() >= 2 && identifier(value, 151, ".-") {
        Ok(value.into())
    } else {
        Err("Use an explicit tunnel name or NAME.CLUSTER identifier".into())
    }
}

fn executable(path: &PathBuf) -> Result<PathBuf> {
    which::which(path).with_context(|| format!("Executable not found: {}", path.display()))
}

fn command(path: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(path);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
}

async fn run_output(path: &PathBuf, args: &[&str], seconds: u64) -> Result<std::process::Output> {
    timeout(
        Duration::from_secs(seconds),
        command(path).args(args).stdin(Stdio::null()).output(),
    )
    .await
    .with_context(|| format!("{} timed out", path.display()))?
    .with_context(|| format!("Failed to run {}", path.display()))
}

fn checked_output(path: &Path, args: &[&str], result: std::process::Output) -> Result<String> {
    ensure!(
        result.status.success(),
        "{} {} failed ({}): {}{}",
        path.display(),
        args.join(" "),
        result.status,
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    String::from_utf8(result.stdout).context("CLI returned non-UTF-8 output")
}

async fn output(path: &PathBuf, args: &[&str], seconds: u64) -> Result<String> {
    checked_output(path, args, run_output(path, args, seconds).await?)
}

async fn lookup_tunnel(path: &PathBuf, tunnel: &str, seconds: u64) -> Result<Option<Value>> {
    let args = ["show", tunnel, "--json"];
    let result = run_output(path, &args, seconds).await?;
    if super::dt::tunnel_not_found(&result, tunnel) {
        return Ok(None);
    }
    let detail = checked_output(path, &args, result)?;
    Ok(Some(
        serde_json::from_str(&detail).context("Invalid tunnel details")?,
    ))
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

async fn doctor(args: DoctorArgs) -> Result<()> {
    let mut checks = Vec::new();
    for (name, path) in [("hc", &args.tools.hc), ("devtunnel", &args.tools.devtunnel)] {
        let result = async {
            let path = executable(path)?;
            let version = output(&path, &["--version"], 45).await?;
            if name == "hc" {
                if args.role != Role::Server {
                    check_via_contract(&path).await?;
                }
                if args.role != Role::Client {
                    ensure!(
                        output(&path, &["connect", "--help"], 45)
                            .await?
                            .contains("--session"),
                        "hc connect must support --session"
                    );
                }
            }
            Ok(format!("{}; {}", version.trim(), path.display()))
        }
        .await;
        checks.push(check(name, result));
    }
    let login = async {
        let path = executable(&args.tools.devtunnel)?;
        let login = output(&path, &["user", "show"], 45).await?;
        ensure!(
            login.to_lowercase().contains("logged in as "),
            "Run devtunnel user login interactively"
        );
        Ok(login.trim().into())
    }
    .await;
    checks.push(check("devtunnel-login", login));
    #[cfg(not(windows))]
    if args.role != Role::Server {
        checks.push(check(
            "sh (required by hc --via)",
            client_shell().map(|p| p.display().to_string()),
        ));
    }
    let ok = checks.iter().all(|c| c.ok);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": ok, "platform": std::env::consts::OS, "checks": checks
            }))?
        );
    } else {
        for check in checks {
            println!(
                "{} {}: {}",
                if check.ok { "OK" } else { "FAIL" },
                check.name,
                check.detail
            );
        }
    }
    ensure!(
        ok,
        "Herdr prerequisites are not ready; no tunnels or machine registrations were changed"
    );
    Ok(())
}

fn check(name: &'static str, result: Result<String>) -> Check {
    match result {
        Ok(detail) => Check {
            name,
            ok: true,
            detail,
        },
        Err(error) => Check {
            name,
            ok: false,
            detail: format!("{error:#}"),
        },
    }
}

async fn setup(args: SetupArgs) -> Result<()> {
    if args.login {
        let status = Command::new(executable(&args.doctor.tools.devtunnel)?)
            .args(["user", "login"])
            .status()
            .await?;
        ensure!(status.success(), "Dev Tunnels login failed: {status}");
    }
    let role = args.doctor.role;
    let json = args.doctor.json;
    doctor(args.doctor).await?;
    if !json {
        println!("Use the SAME Dev Tunnels account and identity provider on both machines.");
        if role != Role::Client {
            println!("Server: hd herdr server --tunnel UNIQUE-NAME");
            println!("The server creates a missing tunnel/port or reuses a valid private tunnel.");
            println!("It runs in the background and returns only after tunnel readiness.");
            println!(
                "Manage: hd herdr status; hd herdr stop (add --name PROFILE for named servers)."
            );
        }
        if role != Role::Server {
            println!("Client: hd herdr client --name remote --tunnel NAME.CLUSTER");
            println!("Verify: hc machine ls; hc exec --on remote whoami");
            println!("Use Herdr App with this local core; do not add the machine through SSH.");
        }
        println!("No dependencies, services, tunnels or machines are installed by setup.");
    }
    Ok(())
}

async fn check_via_contract(hc: &PathBuf) -> Result<()> {
    let help = output(hc, &["machine", "add", "--help"], 45).await?;
    ensure!(help.contains("--via"), "hc machine add must support --via");
    #[cfg(windows)]
    ensure!(
        help.contains("CommandLineToArgvW") && help.contains("no shell"),
        "hc machine add --via must support Windows direct execution (CommandLineToArgvW, no shell)"
    );
    Ok(())
}

#[cfg(any(not(windows), test))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(windows, test))]
fn windows_quote(value: &str) -> String {
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

fn via_path(path: PathBuf) -> Result<String> {
    path.into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("hc --via requires UTF-8 executable paths"))
}

#[cfg(not(windows))]
fn client_shell() -> Result<PathBuf> {
    which::which("sh").context("hc --via needs sh on PATH")
}

async fn add(args: ClientArgs) -> Result<()> {
    let c = args.connection;
    let hc = executable(&c.tools.hc)?;
    let dt = via_path(executable(&c.tools.devtunnel)?)?;
    #[cfg(windows)]
    check_via_contract(&hc).await?;
    #[cfg(not(windows))]
    client_shell()?;
    let exe = via_path(std::env::current_exe()?.canonicalize()?)?;
    let via = [
        exe,
        "herdr".into(),
        "connect".into(),
        "--tunnel".into(),
        c.tunnel,
        "--port".into(),
        c.port.to_string(),
        "--session".into(),
        c.session.clone(),
        "--timeout".into(),
        c.timeout.to_string(),
        "--devtunnel".into(),
        dt,
    ]
    .iter()
    .map(|arg| {
        #[cfg(windows)]
        let quoted = windows_quote(arg);
        #[cfg(not(windows))]
        let quoted = shell_quote(arg);
        quoted
    })
    .collect::<Vec<_>>()
    .join(" ");
    eprintln!(
        "[herdr] Registering {} using a private tunnel, not SSH",
        args.name
    );
    let mut hc = Command::new(hc);
    if let Some(session) = &args.local_session {
        hc.env("HC_SESSION", session).env_remove("HC_SOCKET");
    }
    let status = hc
        .args([
            "machine",
            "add",
            "--name",
            &args.name,
            "--session",
            &c.session,
            "--via",
            &via,
        ])
        .status()
        .await?;
    ensure!(status.success(), "hc machine add failed: {status}");
    Ok(())
}

fn validate_bootstrap(line: &str, session: &str) -> Result<()> {
    let pattern = Regex::new(
        r#"^sh -c 'p=\$\(command -v herdr-core\) \|\| p=\$HOME/\.local/bin/herdr-core; \[ -x "\$p" \] \|\| \{ echo "herdr-core is not installed here" >&2; exit 127; \}; (?:HC_SESSION=([a-zA-Z0-9_.-]+) )?exec "\$p" connect'$"#,
    )?;
    let captures = pattern.captures(line).context(
        "Unsupported hc bootstrap (tested: hc 0.1.17). Install/update hc manually; no supplied command was executed"
    )?;
    ensure!(
        captures.get(1).map_or("default", |v| v.as_str()) == session,
        "hc session does not match the bridge session"
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Hello {
    protocol: String,
    session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
}

async fn read_line(reader: &mut (impl AsyncRead + Unpin), limit: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let byte = reader
            .read_u8()
            .await
            .context("Stream closed before a complete line")?;
        if byte == b'\n' {
            return Ok(line);
        }
        ensure!(line.len() < limit, "Line exceeds {limit} bytes");
        line.push(byte);
    }
}

async fn read_hello(reader: &mut (impl AsyncRead + Unpin), seconds: u64) -> Result<Hello> {
    let bytes = timeout(
        Duration::from_secs(seconds),
        read_line(reader, HANDSHAKE_LIMIT),
    )
    .await
    .context("Bridge handshake timed out")??;
    serde_json::from_slice(&bytes).context("Invalid bridge handshake")
}

async fn write_hello(
    writer: &mut (impl AsyncWrite + Unpin),
    session: &str,
    ok: Option<bool>,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(&Hello {
        protocol: MAGIC.into(),
        session: session.into(),
        ok,
    })?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    Ok(())
}

async fn bridge(
    mut socket: TcpStream,
    hc: PathBuf,
    session: String,
    mut stop: watch::Receiver<bool>,
) -> (Result<()>, Result<()>) {
    let mut child = None;
    let result = async {
        socket.set_nodelay(true)?;
        let hello = tokio::select! {
            biased;
            result = stopped(&mut stop) => return result,
            result = read_hello(&mut socket, 10) => result?,
        };
        ensure!(
            hello.protocol == MAGIC && hello.session == session,
            "Wrong protocol or session"
        );
        if *stop.borrow() {
            return Ok(());
        }
        child = Some(
            command(hc)
                .env_remove("HC_SOCKET")
                .args(["connect", "--session", &session])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,
        );
        let child = child.as_mut().expect("hc was just spawned");
        let mut input = child.stdin.take().context("Missing hc stdin")?;
        let mut output = child.stdout.take().context("Missing hc stdout")?;
        tokio::select! {
            biased;
            result = stopped(&mut stop) => return result,
            result = write_hello(&mut socket, &session, Some(true)) => result?,
        }
        let (mut read, mut write) = socket.into_split();
        let incoming = async {
            tokio::io::copy(&mut read, &mut input).await?;
            drop(input);
            Ok::<_, anyhow::Error>(())
        };
        let outgoing = async {
            tokio::io::copy(&mut output, &mut write).await?;
            write.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        // Preserve half-close responses without letting hc retain a bridge slot
        // forever after client EOF. Output EOF still ends an open-input peer.
        tokio::pin!(incoming);
        tokio::pin!(outgoing);
        tokio::select! {
            biased;
            result = stopped(&mut stop) => result,
            result = &mut incoming => {
                result?;
                tokio::select! {
                    biased;
                    result = stopped(&mut stop) => result,
                    result = timeout(BRIDGE_DRAIN_TIMEOUT, outgoing) => {
                        result.context("hc output drain timed out after client EOF")?
                    }
                }
            }
            result = &mut outgoing => result,
        }
    }
    .await;
    let cleanup = if let Some(mut child) = child {
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            status => {
                let kill = child
                    .start_kill()
                    .context("Failed to stop hc bridge process");
                let wait = timeout(BRIDGE_CLEANUP_TIMEOUT, child.wait())
                    .await
                    .context("Timed out waiting for hc bridge process to exit")
                    .and_then(|result| result.context("Failed to reap hc bridge process"));
                match wait {
                    Ok(_) => Ok(()),
                    Err(error) => combine_results(
                        combine_results(
                            status
                                .map(|_| ())
                                .context("Failed to check hc bridge process"),
                            kill,
                        ),
                        Err(error),
                    ),
                }
            }
        }
    } else {
        Ok(())
    };
    (result, cleanup)
}

fn tunnel_value(value: &Value) -> &Value {
    value.get("tunnel").unwrap_or(value)
}

fn tunnel_id(value: &Value, fallback: &str) -> String {
    let t = tunnel_value(value);
    match (t["tunnelId"].as_str(), t["clusterId"].as_str()) {
        (Some(id), _) if id.contains('.') => id.into(),
        (Some(id), Some(cluster)) => format!("{id}.{cluster}"),
        _ => fallback.into(),
    }
}

fn empty_acl(value: &Value) -> Result<bool> {
    match value {
        Value::Array(entries) => Ok(entries.is_empty()),
        Value::Object(object) => match object.get("entries") {
            Some(Value::Array(entries)) => Ok(entries.is_empty()),
            _ => bail!("Unrecognized tunnel ACL format"),
        },
        _ => bail!("Unrecognized tunnel ACL format"),
    }
}

fn validate_ports(ports: &[Value], port: u16) -> Result<()> {
    ensure!(
        ports.len() <= 1
            && ports
                .iter()
                .all(|p| p["portNumber"].as_u64() == Some(u64::from(port))),
        "Use a dedicated tunnel with only port {port}"
    );
    for acl in ports
        .iter()
        .filter_map(|port| port.get("accessControl"))
        .filter(|acl| !acl.is_null())
    {
        ensure!(
            empty_acl(acl)?,
            "An owner-only private tunnel with no additional tunnel/port access grants is required"
        );
    }
    Ok(())
}

fn validate_tunnel(value: &Value, port: u16) -> Result<()> {
    let t = tunnel_value(value);
    match t.get("ports") {
        Some(Value::Array(ports)) => validate_ports(ports, port)?,
        None | Some(Value::Null) if t["tunnelId"].as_str().is_some() => {}
        _ => bail!("Tunnel has no recognizable ports array"),
    }
    if let Some(acl) = t.get("accessControl").filter(|acl| !acl.is_null()) {
        ensure!(
            empty_acl(acl)?,
            "An owner-only private tunnel with no additional tunnel/port access grants is required"
        );
    }
    Ok(())
}

fn port_list(value: &Value) -> Result<&[Value]> {
    let object = value.as_object().context("Unrecognized tunnel port list")?;
    match (object.get("ports"), object.get("warning"), object.len()) {
        (Some(Value::Array(ports)), None, 1) => Ok(ports),
        (None, Some(Value::String(warning)), 1) if warning == "No ports found for tunnel." => {
            Ok(&[])
        }
        _ => bail!("Unrecognized tunnel port list"),
    }
}

async fn validate_policy(c: &Connection, dt: &PathBuf, port: Option<u16>) -> Result<()> {
    let number = port.map(|port| port.to_string());
    let mut args = vec!["access", "list", &c.tunnel];
    if let Some(number) = &number {
        args.extend(["--port-number", number]);
    }
    args.push("--json");
    let response = output(dt, &args, c.timeout)
        .await
        .context("Failed to retrieve tunnel access policy")?;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct AccessPolicy {
        access_control_entries: Vec<Value>,
        tunnel_id: Option<String>,
        port_number: Option<u16>,
    }

    let _: Value = serde_json::from_str(&response).context("Invalid access policy JSON")?;
    let policy: AccessPolicy = serde_json::from_str(&response).context(
        "Unrecognized access policy: missing accessControlEntries array or unexpected fields",
    )?;
    ensure!(
        policy
            .tunnel_id
            .as_ref()
            .is_none_or(|id| id.eq_ignore_ascii_case(&c.tunnel))
            && policy.port_number.is_none_or(|number| Some(number) == port),
        "Access policy does not match the requested tunnel or port"
    );
    ensure!(
        policy.access_control_entries.is_empty(),
        "An owner-only private tunnel with no additional tunnel/port access grants is required"
    );
    Ok(())
}

async fn validate_private_tunnel(c: &Connection, dt: &PathBuf, detail: &Value) -> Result<bool> {
    validate_tunnel(detail, c.port)?;
    let response = output(dt, &["port", "list", &c.tunnel, "--json"], c.timeout)
        .await
        .context("Failed to retrieve tunnel port list")?;
    let list: Value = serde_json::from_str(&response).context("Invalid tunnel port list JSON")?;
    let ports = port_list(&list)?;
    validate_ports(ports, c.port)?;
    let has_port = !ports.is_empty();
    ensure!(
        has_port
            || !tunnel_value(detail)["ports"]
                .as_array()
                .is_some_and(|ports| !ports.is_empty()),
        "Tunnel details and port list disagree"
    );
    validate_policy(c, dt, None).await?;
    if has_port {
        validate_policy(c, dt, Some(c.port)).await?;
    }
    Ok(has_port)
}

struct Tunnel {
    child: Child,
    lifetime: Option<ChildStdin>,
    lines: tokio::sync::mpsc::Receiver<String>,
}

impl Tunnel {
    fn start(c: &Connection, mode: &str) -> Result<Self> {
        let mut supervisor = command(std::env::current_exe()?);
        // EOF, not killing this supervisor, must reap the official CLI even
        // when the connector itself is abruptly terminated.
        supervisor.kill_on_drop(false);
        let mut child = supervisor
            .args([
                "herdr",
                "_tunnel",
                mode,
                "--tunnel",
                &c.tunnel,
                "--devtunnel",
            ])
            .arg(executable(&c.tools.devtunnel)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let lifetime = child.stdin.take();
        let (sender, lines) = tokio::sync::mpsc::channel(128);
        let stdout = child.stdout.take().context("Missing tunnel stdout")?;
        let stderr = child.stderr.take().context("Missing tunnel stderr")?;
        let ansi = Regex::new(r"\x1b\[[0-9;]*m")?;
        for stream in [
            Box::new(stdout) as Box<dyn AsyncRead + Unpin + Send>,
            Box::new(stderr),
        ] {
            let sender = sender.clone();
            let ansi = ansi.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                loop {
                    match read_line(&mut stream, 65536).await {
                        Ok(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            let line = ansi.replace_all(text.trim_end(), "").into_owned();
                            eprintln!("[herdr] {line}");
                            if sender.send(line).await.is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            log::debug!("Tunnel output ended: {error:#}");
                            break;
                        }
                    }
                }
            });
        }
        Ok(Self {
            child,
            lifetime,
            lines,
        })
    }

    async fn stop(&mut self) -> Result<()> {
        drop(self.lifetime.take());
        match timeout(Duration::from_secs(8), self.child.wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => bail!("Tunnel supervisor did not stop in time"),
        }
        Ok(())
    }
}

async fn tunnel_worker(args: TunnelArgs) -> Result<()> {
    let mut child = command(executable(&args.devtunnel)?)
        .args([&args.mode, &args.tunnel])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut stdin = tokio::io::stdin();
    let mut sink = tokio::io::sink();
    tokio::select! {
        status = child.wait() => {
            ensure!(status?.success(), "devtunnel {} exited unsuccessfully", args.mode);
        }
        result = tokio::io::copy(&mut stdin, &mut sink) => {
            result?;
            child.start_kill()?;
            child.wait().await?;
        }
        result = shutdown_signal() => {
            result?;
            child.start_kill()?;
            child.wait().await?;
        }
    }
    Ok(())
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn prepare_tunnel(c: &mut Connection) -> Result<()> {
    let cluster = c
        .tunnel
        .rsplit_once('.')
        .map(|(_, cluster)| cluster.to_ascii_lowercase());
    if let Some(cluster) = &cluster {
        ensure!(
            !cluster.is_empty() && cluster.bytes().all(|c| c.is_ascii_alphanumeric()),
            "Relay cluster must contain only ASCII letters and digits"
        );
    }
    let dt = executable(&c.tools.devtunnel)?;
    output(&dt, &["user", "show"], c.timeout).await?;
    let mut detail = if let Some(detail) = lookup_tunnel(&dt, &c.tunnel, c.timeout).await? {
        detail
    } else {
        let name = c
            .tunnel
            .rsplit_once('.')
            .map_or(c.tunnel.as_str(), |(name, _)| name);
        let service_uri = cluster
            .as_ref()
            .map(|cluster| format!("https://{cluster}.rel.tunnels.api.visualstudio.com"));
        let mut args = vec!["create", name];
        if let Some(service_uri) = &service_uri {
            args.extend(["--service-uri", service_uri]);
        }
        args.push("--json");
        let created: Value = serde_json::from_str(&output(&dt, &args, c.timeout).await?)?;
        c.tunnel = tunnel_id(&created, &c.tunnel);
        eprintln!(
            "[herdr] Created private tunnel {}; it remains after exit",
            c.tunnel
        );
        lookup_tunnel(&dt, &c.tunnel, c.timeout)
            .await?
            .context("Created tunnel was not found; no port was published")?
    };
    c.tunnel = tunnel_id(&detail, &c.tunnel);
    if let Some(cluster) = cluster {
        ensure!(
            c.tunnel
                .rsplit_once('.')
                .is_some_and(|(_, actual)| actual.eq_ignore_ascii_case(&cluster)),
            "Requested relay {cluster}, but the service returned {}; no port was published",
            c.tunnel
        );
    }
    if !validate_private_tunnel(c, &dt, &detail).await? {
        eprintln!(
            "[herdr] Adding port {} to private tunnel {}",
            c.port, c.tunnel
        );
        output(
            &dt,
            &[
                "port",
                "create",
                &c.tunnel,
                "--port-number",
                &c.port.to_string(),
                "--protocol",
                "auto",
                "--json",
            ],
            c.timeout,
        )
        .await?;
        detail = lookup_tunnel(&dt, &c.tunnel, c.timeout)
            .await?
            .context("Tunnel disappeared after creating its port")?;
        ensure!(
            validate_private_tunnel(c, &dt, &detail).await?,
            "Tunnel is missing port {}",
            c.port
        );
    }
    Ok(())
}

async fn serve(args: ServerArgs, reporter: &background::Reporter) -> Result<()> {
    let matchers = super::dt::build_failure_matchers(&args.patterns)?;
    let mut c = args.connection.clone();
    let hc = executable(&c.tools.hc)?;
    // Bind before publishing, so a busy port never exposes an unrelated service.
    let listener = TcpListener::bind(("127.0.0.1", c.port)).await?;
    reporter.update(&c, false, 0, "starting")?;
    let shutdown = shutdown_signal();
    let requested = reporter.stop_requested();
    tokio::pin!(shutdown, requested);
    tokio::select! {
        result = prepare_tunnel(&mut c) => result?,
        result = &mut shutdown => return result,
        result = &mut requested => return result,
    }
    eprintln!(
        "[herdr] Bridge listening on 127.0.0.1:{}; hc session={}",
        c.port, c.session
    );
    eprintln!(
        "[herdr] Tunnel clients and trusted local processes get full hc access as this user."
    );
    let (stop, receiver) = watch::channel(false);
    let guard = guard_host(c.clone(), &args, &matchers, reporter, receiver.clone());
    let bridge = accept_connections(listener, hc, c.session, stop.clone(), receiver);
    tokio::pin!(guard, bridge);
    let (result, guard_done, bridge_done) = tokio::select! {
        result = &mut guard => (result, true, false),
        result = &mut bridge => (result, false, true),
        result = &mut shutdown => (result, false, false),
        result = &mut requested => (result, false, false),
    };
    stop.send_replace(true);
    let (guard_result, bridge_result) = match (guard_done, bridge_done) {
        (true, false) => (Ok(()), bridge.await),
        (false, true) => (guard.await, Ok(())),
        (false, false) => tokio::join!(guard, bridge),
        (true, true) => unreachable!(),
    };
    combine_results(combine_results(result, guard_result), bridge_result)
}

fn combine_results(first: Result<()>, second: Result<()>) -> Result<()> {
    match (first, second) {
        (Err(first), Err(second)) => Err(first.context(format!("Additional error: {second:#}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn accept_connections(
    listener: TcpListener,
    hc: PathBuf,
    session: String,
    stop: watch::Sender<bool>,
    mut receiver: watch::Receiver<bool>,
) -> Result<()> {
    let permits = Arc::new(Semaphore::new(32));
    let mut connections = JoinSet::new();
    let mut result = loop {
        tokio::select! {
            biased;
            result = stopped(&mut receiver) => break result,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result.context("Bridge task failed").and_then(|result| result) {
                    break Err(error);
                }
            }
            accepted = listener.accept() => {
                let (socket, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error.into()),
                };
                match permits.clone().try_acquire_owned() {
                    Ok(permit) => {
                        let hc = hc.clone();
                        let session = session.clone();
                        let receiver = receiver.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            let (result, cleanup) = bridge(socket, hc, session, receiver).await;
                            match (result, cleanup) {
                                (Err(error), Err(cleanup)) => {
                                    Err(cleanup.context(format!("Bridge failed: {error:#}")))
                                }
                                (Err(error), Ok(())) => {
                                    eprintln!("[herdr] Bridge rejected/closed: {error:#}");
                                    Ok(())
                                }
                                (Ok(()), cleanup) => cleanup,
                            }
                        });
                    }
                    Err(_) => eprintln!("[herdr] Rejected connection: bridge connection limit reached"),
                }
            }
        }
    };
    drop(listener);
    stop.send_replace(true);
    while let Some(joined) = connections.join_next().await {
        result = combine_results(
            result,
            joined.context("Bridge task failed").and_then(|result| result),
        );
    }
    result
}

async fn stopped(stop: &mut watch::Receiver<bool>) -> Result<()> {
    stop.wait_for(|requested| *requested)
        .await
        .context("Server shutdown channel closed")?;
    Ok(())
}

async fn guard_host(
    mut c: Connection,
    args: &ServerArgs,
    matchers: &[Regex],
    reporter: &background::Reporter,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut restarts = 0u32;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let attempt = if restarts != 0 {
            tokio::select! {
                result = prepare_tunnel(&mut c) => result.and_then(|_| Tunnel::start(&c, "host")),
                result = stopped(&mut stop) => return result,
            }
        } else {
            Tunnel::start(&c, "host")
        };
        let result = match attempt {
            Ok(mut tunnel) => {
                let result =
                    watch_host(&mut tunnel, &c, restarts, matchers, reporter, &mut stop).await;
                tunnel
                    .stop()
                    .await
                    .context("Tunnel cleanup failed; refusing to launch an overlapping host")?;
                result
            }
            Err(error) => Err(error),
        };
        if *stop.borrow() {
            return Ok(());
        }
        let error = match result {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        reporter.update(&c, false, restarts, &format!("recovering: {error:#}"))?;
        eprintln!("[herdr] Tunnel host failed: {error:#}");
        if args.max_restarts != 0 && restarts >= args.max_restarts {
            return Err(error).context(format!("Reached max restarts ({})", args.max_restarts));
        }
        restarts = restarts.saturating_add(1);
        reporter.update(&c, false, restarts, "waiting to restart tunnel host")?;
        eprintln!(
            "[herdr] Restarting tunnel host (attempt #{restarts}) after {}s cooldown",
            args.cooldown
        );
        tokio::select! {
            result = stopped(&mut stop) => return result,
            _ = tokio::time::sleep(Duration::from_secs(args.cooldown)) => {}
        }
    }
}

async fn watch_host(
    tunnel: &mut Tunnel,
    c: &Connection,
    restarts: u32,
    matchers: &[Regex],
    reporter: &background::Reporter,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    reporter.update(c, false, restarts, "waiting for tunnel readiness")?;
    let deadline = tokio::time::sleep(Duration::from_secs(c.timeout));
    tokio::pin!(deadline);
    let mut ready = false;
    loop {
        tokio::select! {
            result = stopped(stop) => return result,
            status = tunnel.child.wait() => bail!("devtunnel host exited unexpectedly: {}", status?),
            _ = &mut deadline, if !ready => bail!("devtunnel host readiness timed out"),
            line = tunnel.lines.recv() => {
                let line = line.context("devtunnel host output closed")?;
                ensure!(!super::dt::line_indicates_failure(matchers, &line),
                    "Detected devtunnel failure signature: {line}");
                if !ready && line.contains("Ready to accept connections for tunnel:") {
                    ready = true;
                    reporter.update(c, true, restarts, "ready")?;
                    eprintln!("[herdr] CLIENT COMMAND: hd herdr client --name remote --tunnel {} --port {} --session {}", c.tunnel, c.port, c.session);
                }
            }
        }
    }
}

fn forwarding_port(line: &str, remote: u16) -> Option<u16> {
    let captures = Regex::new(r"Forwarding from 127\.0\.0\.1:(\d+) to host port (\d+)\.")
        .ok()?
        .captures(line)?;
    if captures[2].parse::<u16>().ok()? != remote {
        return None;
    }
    captures[1].parse::<u16>().ok().filter(|p| *p != 0)
}

async fn connect(args: ConnectArgs) -> Result<()> {
    let c = args.connection;
    validate_bootstrap(&args.bootstrap, &c.session)?;
    let mut tunnel = Tunnel::start(&c, "connect")?;
    let result = async {
        let port = timeout(Duration::from_secs(c.timeout), async {
            let mut mapped = None;
            loop {
                let line = tunnel
                    .lines
                    .recv()
                    .await
                    .context("devtunnel connect closed before readiness")?;
                mapped = forwarding_port(&line, c.port).or(mapped);
                if let Some(port) = mapped
                    && line.contains(&format!(
                        "PortForwardingService listening on 127.0.0.1:{port}."
                    ))
                {
                    return Ok::<_, anyhow::Error>(port);
                }
            }
        })
        .await
        .context(
            "No matching devtunnel local port; check host, login, port and CLI output format",
        )??;
        let mut socket = timeout(
            Duration::from_secs(10),
            TcpStream::connect(("127.0.0.1", port)),
        )
        .await??;
        socket.set_nodelay(true)?;
        write_hello(&mut socket, &c.session, None).await?;
        let reply = read_hello(&mut socket, c.timeout).await?;
        ensure!(
            reply.protocol == MAGIC && reply.session == c.session && reply.ok == Some(true),
            "Invalid bridge handshake"
        );
        let (mut read, mut write) = socket.into_split();
        let send = async {
            tokio::io::copy(&mut tokio::io::stdin(), &mut write).await?;
            write.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        let receive = async {
            let mut stdout = tokio::io::stdout();
            tokio::io::copy(&mut read, &mut stdout).await?;
            stdout.flush().await?;
            Ok::<_, anyhow::Error>(())
        };
        tokio::pin!(send);
        tokio::pin!(receive);
        let transfer = async {
            tokio::select! {
                result = &mut send => { result?; receive.await }
                result = &mut receive => result,
            }
        };
        tokio::pin!(transfer);
        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                result = &mut transfer => return result,
                status = tunnel.child.wait() => bail!("devtunnel connect exited: {}", status?),
                result = &mut shutdown => return result,
                line = tunnel.lines.recv() => {
                    ensure!(line.is_some(), "devtunnel connect output closed");
                }
            }
        }
    }
    .await;
    let cleanup = tunnel.stop().await;
    result.and(cleanup)
}
