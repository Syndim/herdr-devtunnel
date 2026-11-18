use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::timeout;

use super::protocol::{MAGIC, read_hello, write_hello};
use super::supervisor::{BridgeActivity, stopped};
use crate::process::{combine_results, command};

const BRIDGE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const STDERR_LINE_LIMIT: usize = 65536;

async fn log_child_stderr(mut stream: impl AsyncRead + Unpin, id: u64) {
    let mut chunk = [0; 8192];
    let mut line = Vec::new();
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(count) => {
                for &byte in &chunk[..count] {
                    if byte == b'\n' {
                        log_child_line(id, &line);
                        line.clear();
                    } else {
                        line.push(byte);
                        if line.len() == STDERR_LINE_LIMIT {
                            let split = match std::str::from_utf8(&line) {
                                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                                _ => line.len(),
                            };
                            log_child_line(id, &line[..split]);
                            line.drain(..split);
                        }
                    }
                }
            }
            Err(error) => {
                log::warn!(target: "herdr_events", "[herdr] Bridge #{id} hc stderr read failed: {error}");
                break;
            }
        }
    }
    if !line.is_empty() {
        log_child_line(id, &line);
    }
}

fn log_child_line(id: u64, bytes: &[u8]) {
    let line = String::from_utf8_lossy(bytes);
    log::info!(target: "herdr_events", "[herdr] Bridge #{id} hc: {}", line.trim_end_matches('\r'));
}

pub(super) async fn bridge(
    mut socket: TcpStream,
    hc: PathBuf,
    session: String,
    activity: &BridgeActivity,
    mut stop: watch::Receiver<bool>,
) -> (Result<()>, Result<()>) {
    let mut child = None;
    let mut stderr_task = None;
    let mut reason = "handshake or setup failed";
    let result = async {
        socket.set_nodelay(true)?;
        let hello = tokio::select! {
            biased;
            result = stopped(&mut stop) => {
                reason = "server shutdown";
                return result;
            },
            result = read_hello(&mut socket, 10) => result?,
        };
        ensure!(
            hello.protocol == MAGIC && hello.session == session,
            "Wrong protocol or session"
        );
        activity.set_stage("setup");
        if *stop.borrow() {
            reason = "server shutdown";
            return Ok(());
        }
        child = Some(
            command(hc)
                .env_remove("HC_SOCKET")
                .args(["connect", "--session", &session])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let child = child.as_mut().expect("hc was just spawned");
        let mut input = child.stdin.take().context("Missing hc stdin")?;
        let mut output = child.stdout.take().context("Missing hc stdout")?;
        let stderr = child.stderr.take().context("Missing hc stderr")?;
        stderr_task = Some(tokio::spawn(log_child_stderr(stderr, activity.id())));
        tokio::select! {
            biased;
            result = stopped(&mut stop) => {
                reason = "server shutdown";
                return result;
            },
            result = write_hello(&mut socket, &session, Some(true)) => result?,
        }
        activity.set_stage("forwarding");
        reason = "forwarding failed";
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
            result = stopped(&mut stop) => {
                reason = "server shutdown";
                result
            },
            result = &mut incoming => {
                reason = "client input failed";
                result?;
                reason = "client EOF";
                activity.set_stage("draining");
                tokio::select! {
                    biased;
                    result = stopped(&mut stop) => {
                        reason = "server shutdown";
                        result
                    },
                    result = timeout(BRIDGE_DRAIN_TIMEOUT, outgoing) => {
                        reason = "client EOF; hc output drain";
                        result.context("hc output drain timed out after client EOF")?
                    }
                }
            }
            result = &mut outgoing => {
                reason = if result.is_ok() { "hc output closed" } else { "hc output failed" };
                result
            },
        }
    }
    .await;
    activity.set_stage("cleanup");
    let cleanup_deadline = tokio::time::Instant::now() + BRIDGE_CLEANUP_TIMEOUT;
    let cleanup = if let Some(mut child) = child {
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            status => {
                let kill = child
                    .start_kill()
                    .context("Failed to stop hc bridge process");
                let wait = tokio::time::timeout_at(cleanup_deadline, child.wait())
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
    if let Some(mut task) = stderr_task {
        if tokio::time::timeout_at(cleanup_deadline, &mut task)
            .await
            .is_err()
        {
            task.abort();
            log::warn!(target: "herdr_events",
                "[herdr] Bridge #{} hc stderr drain timed out",
                activity.id()
            );
        }
    }
    log::info!(target: "herdr_events",
        "[herdr] Bridge #{} closed: reason={reason}; age={}s; result={}; cleanup={}",
        activity.id(),
        activity.age().as_secs(),
        if result.is_ok() { "ok" } else { "error" },
        if cleanup.is_ok() { "ok" } else { "error" }
    );
    (result, cleanup)
}
