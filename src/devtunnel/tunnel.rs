use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use serde_json::Value;
use tokio::io::AsyncRead;
use tokio::process::{Child, ChildStdin};
use tokio::time::timeout;

use crate::config::{Connection, TunnelArgs, executable};
use crate::devtunnel::policy::{tunnel_id, validate_private_tunnel};
use crate::herdr::protocol::read_line;
use crate::process::{command, lookup_tunnel, output};
pub(crate) struct Tunnel {
    pub(crate) child: Child,
    lifetime: Option<ChildStdin>,
    pub(crate) lines: tokio::sync::mpsc::Receiver<String>,
}

impl Tunnel {
    pub(crate) fn start(c: &Connection, mode: &str) -> Result<Self> {
        let mut supervisor = command(std::env::current_exe()?);
        // EOF, not killing this supervisor, must reap the official CLI even
        // when the connector itself is abruptly terminated.
        supervisor.kill_on_drop(false);
        let mut child = supervisor
            .args(["_tunnel", mode, "--tunnel", &c.tunnel, "--devtunnel"])
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

    pub(crate) async fn stop(&mut self) -> Result<()> {
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

pub(crate) async fn tunnel_worker(args: TunnelArgs) -> Result<()> {
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

pub(crate) async fn shutdown_signal() -> Result<()> {
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

pub(crate) async fn prepare_tunnel(c: &mut Connection) -> Result<()> {
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
