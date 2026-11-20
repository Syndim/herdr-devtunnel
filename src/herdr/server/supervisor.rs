use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

use super::bridge::bridge;
use super::protocol::{MAGIC, read_hello, validate_bootstrap, write_hello};
use crate::background::Reporter;
use crate::devtunnel::process::{Tunnel, prepare_tunnel};
use crate::herdr::config::{ConnectArgs, Connection, ServerArgs};
use crate::herdr::validation::executable;
use crate::process::combine_results;
use crate::process::shutdown::shutdown_signal;
struct BridgeInfo {
    started: Instant,
    stage: &'static str,
}

pub(super) struct BridgeActivity {
    id: u64,
    started: Instant,
    entries: Arc<Mutex<BTreeMap<u64, BridgeInfo>>>,
}

impl BridgeActivity {
    fn new(id: u64, entries: Arc<Mutex<BTreeMap<u64, BridgeInfo>>>) -> Self {
        let started = Instant::now();
        entries.lock().unwrap().insert(
            id,
            BridgeInfo {
                started,
                stage: "handshake",
            },
        );
        Self {
            id,
            started,
            entries,
        }
    }

    pub(super) fn set_stage(&self, stage: &'static str) {
        if let Some(info) = self.entries.lock().unwrap().get_mut(&self.id) {
            info.stage = stage;
        }
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn age(&self) -> Duration {
        self.started.elapsed()
    }
}

impl Drop for BridgeActivity {
    fn drop(&mut self) {
        self.entries.lock().unwrap().remove(&self.id);
    }
}

pub(crate) async fn serve(args: ServerArgs, reporter: &Reporter) -> Result<()> {
    let matchers = crate::devtunnel::build_failure_matchers(&args.patterns)?;
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
    log::info!(target: "herdr_events",
        "[herdr] Bridge listening on 127.0.0.1:{}; hc session={}",
        c.port,
        c.session
    );
    log::info!(target: "herdr_events",
        "[herdr] Tunnel clients and trusted local processes get full hc access as this user."
    );
    let (stop, receiver) = watch::channel(false);
    let guard = guard_host(c.clone(), &args, &matchers, reporter, receiver.clone());
    let bridge = accept_connections(
        listener,
        hc,
        c.session,
        usize::from(args.max_bridge_connections),
        Duration::from_secs(args.bridge_write_timeout),
        stop.clone(),
        receiver,
    );
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

async fn accept_connections(
    listener: TcpListener,
    hc: PathBuf,
    session: String,
    limit: usize,
    write_timeout: Duration,
    stop: watch::Sender<bool>,
    mut receiver: watch::Receiver<bool>,
) -> Result<()> {
    let permits = Arc::new(Semaphore::new(limit));
    let entries = Arc::new(Mutex::new(BTreeMap::new()));
    let mut next_id = 0u64;
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
                        next_id = next_id.wrapping_add(1);
                        let activity = BridgeActivity::new(next_id, entries.clone());
                        log::info!(target: "herdr_events", "[herdr] Bridge #{} accepted", activity.id());
                        connections.spawn(async move {
                            let _permit = permit;
                            let (result, cleanup) = bridge(socket, hc, session, &activity, receiver, write_timeout).await;
                            match (result, cleanup) {
                                (Err(error), Err(cleanup)) => {
                                    Err(cleanup.context(format!("Bridge #{} failed: {error:#}", activity.id())))
                                }
                                (Err(error), Ok(())) => {
                                    log::info!(target: "herdr_events", "[herdr] Bridge #{} rejected/closed: {error:#}", activity.id());
                                    Ok(())
                                }
                                (Ok(()), cleanup) => cleanup,
                            }
                        });
                    }
                    Err(_) => {
                        let (tracked, handshake, setup, forwarding, draining, cleanup, oldest) = {
                            let entries = entries.lock().unwrap();
                            let count = |stage| entries.values().filter(|info| info.stage == stage).count();
                            (entries.len(), count("handshake"), count("setup"), count("forwarding"), count("draining"), count("cleanup"),
                             entries.values().map(|info| info.started.elapsed().as_secs()).max().unwrap_or(0))
                        };
                        log::info!(target: "herdr_events", "[herdr] Rejected connection: bridge connection limit reached (limit={limit}; tracked={tracked}; handshake={handshake}; setup={setup}; forwarding={forwarding}; draining={draining}; cleanup={cleanup}; oldest={oldest}s)");
                    },
                }
            }
        }
    };
    drop(listener);
    stop.send_replace(true);
    while let Some(joined) = connections.join_next().await {
        result = combine_results(
            result,
            joined
                .context("Bridge task failed")
                .and_then(|result| result),
        );
    }
    result
}

pub(super) async fn stopped(stop: &mut watch::Receiver<bool>) -> Result<()> {
    stop.wait_for(|requested| *requested)
        .await
        .context("Server shutdown channel closed")?;
    Ok(())
}

async fn guard_host(
    mut c: Connection,
    args: &ServerArgs,
    matchers: &[Regex],
    reporter: &Reporter,
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
        log::info!(target: "herdr_events", "[herdr] Tunnel host failed: {error:#}");
        if args.max_restarts != 0 && restarts >= args.max_restarts {
            return Err(error).context(format!("Reached max restarts ({})", args.max_restarts));
        }
        restarts = restarts.saturating_add(1);
        reporter.update(&c, false, restarts, "waiting to restart tunnel host")?;
        log::info!(target: "herdr_events",
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
    reporter: &Reporter,
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
                ensure!(!crate::devtunnel::line_indicates_failure(matchers, &line),
                    "Detected devtunnel failure signature: {line}");
                if !ready && line.contains("Ready to accept connections for tunnel:") {
                    ready = true;
                    reporter.update(c, true, restarts, "ready")?;
                    log::info!(target: "herdr_events", "[herdr] CLIENT COMMAND: hd client connect --name remote --tunnel {} --port {} --session {}", c.tunnel, c.port, c.session);
                }
            }
        }
    }
}

pub(crate) fn forwarding_port(line: &str, remote: u16) -> Option<u16> {
    let captures = Regex::new(r"Forwarding from 127\.0\.0\.1:(\d+) to host port (\d+)\.")
        .ok()?
        .captures(line)?;
    if captures[2].parse::<u16>().ok()? != remote {
        return None;
    }
    captures[1].parse::<u16>().ok().filter(|p| *p != 0)
}

pub(crate) async fn connect(args: ConnectArgs) -> Result<()> {
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
