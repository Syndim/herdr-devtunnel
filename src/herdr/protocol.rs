use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::timeout;

use super::process::{combine_results, command};
use super::server_runtime::stopped;

pub(super) const MAGIC: &str = "herdr-devtunnel/1";
pub(super) const HANDSHAKE_LIMIT: usize = 4096;
const BRIDGE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) fn validate_bootstrap(line: &str, session: &str) -> Result<()> {
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
pub(super) struct Hello {
    pub(super) protocol: String,
    pub(super) session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) ok: Option<bool>,
}

pub(super) async fn read_line(
    reader: &mut (impl AsyncRead + Unpin),
    limit: usize,
) -> Result<Vec<u8>> {
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

pub(super) async fn read_hello(
    reader: &mut (impl AsyncRead + Unpin),
    seconds: u64,
) -> Result<Hello> {
    let bytes = timeout(
        Duration::from_secs(seconds),
        read_line(reader, HANDSHAKE_LIMIT),
    )
    .await
    .context("Bridge handshake timed out")??;
    serde_json::from_slice(&bytes).context("Invalid bridge handshake")
}

pub(super) async fn write_hello(
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

pub(super) async fn bridge(
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
