use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};

use super::{Connection, ServerArgs, combine_results, executable, parse_machine, serve};
use crate::dt::{background as lifecycle, build_failure_matchers};

#[derive(Args)]
pub struct SessionArgs {
    /// Profile supplied to `hd herdr server --name`.
    #[arg(long, default_value = "default", value_parser = parse_machine)]
    name: String,
}

#[derive(Args)]
pub struct WorkerArgs {
    #[arg(long)]
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct State {
    pid: u32,
    tunnel: String,
    port: u16,
    session: String,
    ready: bool,
    restarts: u32,
    message: String,
}

pub(super) struct Reporter {
    directory: Option<PathBuf>,
}

impl Reporter {
    pub(super) fn foreground() -> Self {
        Self { directory: None }
    }

    pub(super) fn update(
        &self,
        c: &Connection,
        ready: bool,
        restarts: u32,
        message: &str,
    ) -> Result<()> {
        if let Some(directory) = &self.directory {
            write_state(
                directory,
                &State {
                    pid: std::process::id(),
                    tunnel: c.tunnel.clone(),
                    port: c.port,
                    session: c.session.clone(),
                    ready,
                    restarts,
                    message: message.into(),
                },
            )?;
        }
        Ok(())
    }

    pub(super) async fn stop_requested(&self) -> Result<()> {
        let Some(directory) = &self.directory else {
            return std::future::pending().await;
        };
        loop {
            if directory
                .join("stop")
                .try_exists()
                .context("Failed to inspect Herdr stop request")?
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn finish(&self, result: &Result<()>) -> Result<()> {
        if let Some(directory) = &self.directory {
            let mut state = read_state(directory)?.context("Missing managed Herdr state")?;
            state.ready = false;
            state.message = match result {
                Ok(()) => "stopped".into(),
                Err(error) => format!("failed: {error:#}"),
            };
            write_state(directory, &state)?;
        }
        Ok(())
    }
}

fn runtime_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    let home = home
        .filter(|value| !value.is_empty())
        .context("Cannot locate the user's home directory")?;
    Ok(PathBuf::from(home).join(".herdr-devtunnel"))
}

fn directory(name: &str) -> Result<PathBuf> {
    let directory = runtime_directory()?.join(format!("server-{}", name.to_ascii_lowercase()));
    lifecycle::create_directory(&directory)?;
    directory
        .canonicalize()
        .context("Failed to resolve Herdr server directory")
}

fn write_state(directory: &Path, state: &State) -> Result<()> {
    let temporary = directory.join("state.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)
        .context("Failed to write Herdr state")?;
    fs::rename(temporary, directory.join("state.json")).context("Failed to publish Herdr state")
}

fn read_state(directory: &Path) -> Result<Option<State>> {
    let bytes = match fs::read(directory.join("state.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Failed to read Herdr state"),
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("Invalid Herdr state")?,
    ))
}

pub(super) fn start(mut args: ServerArgs) -> Result<()> {
    build_failure_matchers(&args.patterns)?;
    args.name.make_ascii_lowercase();
    args.connection.tools.hc = executable(&args.connection.tools.hc)?.canonicalize()?;
    args.connection.tools.devtunnel =
        executable(&args.connection.tools.devtunnel)?.canonicalize()?;
    let directory = directory(&args.name)?;
    let management = lifecycle::management_lock(&directory)?;
    start_locked(args, &directory, management)
}

fn startup_timeout(timeout: u64, cooldown: u64, max_restarts: u32) -> Duration {
    let retries = u64::from(max_restarts.max(1));
    let attempts = retries.saturating_add(1);
    let seconds = timeout
        .saturating_mul(12)
        .saturating_mul(attempts)
        .saturating_add(cooldown.saturating_add(8).saturating_mul(retries))
        .saturating_add(60);
    Duration::from_secs(seconds)
}

fn start_locked(args: ServerArgs, directory: &Path, management: fs::File) -> Result<()> {
    if lifecycle::running(directory)? {
        let configured: ServerArgs =
            serde_json::from_slice(&fs::read(directory.join("config.json"))?)
                .context("Invalid running Herdr configuration")?;
        ensure!(
            configured == args,
            "Server '{}' is running with different settings; stop it explicitly before reconfiguring",
            args.name
        );
        ensure!(
            !directory.join("stop").try_exists()?,
            "Server '{}' has a shutdown request pending; shutdown is cooperative and may take time. Check `hd herdr status --name {}` and retry `hd herdr stop --name {}` if needed; then retry `server`. See {}",
            args.name,
            args.name,
            args.name,
            directory.join("server.log").display()
        );
        let state = read_state(directory)?;
        let Some(state) = state.filter(|state| state.ready) else {
            bail!(
                "Server '{}' is running but not ready (it may still be starting or recovering). It remains active and may retry in the background. Check `hd herdr status --name {}` and retry `server` when ready, or request shutdown with `hd herdr stop --name {}`. See {}",
                args.name,
                args.name,
                args.name,
                directory.join("server.log").display()
            );
        };
        println!(
            "Server '{}' is already running; leaving it unchanged.",
            args.name
        );
        show_running_status(&args.name, directory, &state);
        return Ok(());
    }
    lifecycle::sweep_snapshots(directory)?;
    lifecycle::remove_if_present(&directory.join("stop"))?;
    lifecycle::remove_if_present(&directory.join("state.json"))?;
    let snapshot = directory.join(format!(
        "worker-{:032x}{}",
        rand::random::<u128>(),
        std::env::consts::EXE_SUFFIX
    ));
    fs::copy(std::env::current_exe()?, &snapshot)
        .context("Failed to copy the Herdr server executable")?;
    fs::write(
        directory.join("config.json"),
        serde_json::to_vec_pretty(&args)?,
    )
    .context("Failed to write Herdr server configuration")?;
    let log_path = directory.join("server.log");
    let log = fs::File::create(&log_path).context("Failed to create Herdr server log")?;
    let mut command = Command::new(snapshot);
    command
        .args(["herdr", "server-worker", "--directory"])
        .arg(directory)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    lifecycle::detach(&mut command)?;
    let mut child = command
        .spawn()
        .context("Failed to start managed Herdr server")?;
    let mut management = Some(management);
    let startup_timeout =
        startup_timeout(args.connection.timeout, args.cooldown, args.max_restarts);
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("Failed to poll Herdr startup")? {
            bail!(
                "Server exited during startup ({status}). See {}",
                log_path.display()
            );
        }
        if let Some(state) = read_state(directory)?
            && state.pid == child.id()
            && lifecycle::running(directory)?
        {
            drop(management.take());
            if state.ready {
                println!(
                    "Server '{}' started in background (PID {}).",
                    args.name,
                    child.id()
                );
                println!(
                    "CLIENT COMMAND: hd herdr client --name remote --tunnel {} --port {} --session {}",
                    state.tunnel, state.port, state.session
                );
                println!("Log: {}", log_path.display());
                return Ok(());
            }
        }
        if started.elapsed() >= startup_timeout {
            fs::write(directory.join("stop"), b"").context("Failed to cancel Herdr startup")?;
            bail!(
                "Herdr startup timed out; shutdown was requested but is not confirmed. Shutdown is cooperative and may take time. Check `hd herdr status --name {}` and retry `hd herdr stop --name {}` if needed. See {}",
                args.name,
                args.name,
                log_path.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn show_running_status(name: &str, directory: &Path, state: &State) {
    println!(
        "Server '{name}' is running (PID {}); ready={}; restarts={}; {}.",
        state.pid, state.ready, state.restarts, state.message
    );
    println!(
        "Tunnel: {}; port={}; session={}",
        state.tunnel, state.port, state.session
    );
    println!("Log: {}", directory.join("server.log").display());
}

fn show_status(name: &str, directory: &Path) -> Result<()> {
    let running = lifecycle::running(directory)?;
    if running {
        let state = read_state(directory)?.context("Running Herdr server has no state")?;
        show_running_status(name, directory, &state);
    } else {
        println!("Server '{name}' is not running.");
        if let Some(state) = read_state(directory)? {
            println!("Last status: {}", state.message);
        }
        println!("Log: {}", directory.join("server.log").display());
    }
    Ok(())
}

pub(super) fn status(args: SessionArgs) -> Result<()> {
    let directory = directory(&args.name)?;
    let _management = lifecycle::management_lock(&directory)?;
    show_status(&args.name, &directory)
}

pub(super) fn stop(args: SessionArgs) -> Result<()> {
    let directory = directory(&args.name)?;
    let _management = lifecycle::management_lock(&directory)?;
    stop_in_directory(&args.name, &directory)
}

fn stop_in_directory(name: &str, directory: &Path) -> Result<()> {
    if !lifecycle::running(directory)? {
        println!("Server '{name}' is not running.");
        return Ok(());
    }
    fs::write(directory.join("stop"), b"").context("Failed to request Herdr shutdown")?;
    let started = Instant::now();
    while lifecycle::running(directory)? {
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Server '{}' has not stopped after 10 seconds; shutdown remains requested but is not confirmed. Shutdown is cooperative and may take time. Check `hd herdr status --name {}` and retry `hd herdr stop --name {}` if needed. See {}",
            name,
            name,
            name,
            directory.join("server.log").display()
        );
        thread::sleep(Duration::from_millis(100));
    }
    let state = read_state(directory)?.with_context(|| {
        format!(
            "Server '{name}' exited without a final status; shutdown is not confirmed. See {}",
            directory.join("server.log").display()
        )
    })?;
    ensure!(
        !state.ready && state.message == "stopped",
        "Server '{name}' did not stop cleanly: {}. See {}",
        state.message,
        directory.join("server.log").display()
    );
    println!("Server '{name}' stopped.");
    Ok(())
}

pub(super) fn reconnect_all() -> Result<()> {
    let entries = match fs::read_dir(runtime_directory()?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("No running managed Herdr servers found.");
            return Ok(());
        }
        Err(error) => return Err(error).context("Failed to enumerate managed Herdr servers"),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Some(name) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("server-"))
        {
            names.push(parse_machine(name).map_err(anyhow::Error::msg)?);
        }
    }
    names.sort();
    let mut count = 0;
    let mut failures = Vec::new();
    for name in names {
        let result = (|| {
            let directory = directory(&name)?;
            let management = lifecycle::management_lock(&directory)?;
            if !lifecycle::running(&directory)? {
                return Ok(());
            }
            count += 1;
            let args: ServerArgs =
                serde_json::from_slice(&fs::read(directory.join("config.json"))?)
                    .context("Invalid managed Herdr configuration")?;
            ensure!(
                args.name == name && !args.foreground,
                "Managed Herdr configuration does not match profile '{name}'"
            );
            executable(&args.connection.tools.hc)?;
            executable(&args.connection.tools.devtunnel)?;
            build_failure_matchers(&args.patterns)?;
            println!("Reconnecting server '{name}'...");
            stop_in_directory(&name, &directory)?;
            start_locked(args, &directory, management)
        })();
        if let Err(error) = result {
            eprintln!("[herdr] Server '{name}': {error:#}");
            failures.push(format!("{name}: {error:#}"));
        }
    }
    if count == 0 && failures.is_empty() {
        println!("No running managed Herdr servers found.");
    }
    ensure!(
        failures.is_empty(),
        "Failed to reconnect {} server(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

pub(super) async fn worker(args: WorkerArgs) -> Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    ensure!(
        executable.parent() == Some(args.directory.as_path())
            && executable
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("worker-")),
        "server-worker must run from a private executable snapshot; use hd herdr server"
    );
    let _worker = lifecycle::try_lock(&args.directory.join("worker.lock"))?
        .context("Herdr server is already running")?;
    let config: ServerArgs = serde_json::from_slice(&fs::read(args.directory.join("config.json"))?)
        .context("Invalid Herdr server configuration")?;
    let reporter = Reporter {
        directory: Some(args.directory),
    };
    reporter.update(&config.connection, false, 0, "starting")?;
    let result = serve(config, &reporter).await;
    let status = reporter.finish(&result);
    combine_results(result, status)
}

#[cfg(test)]
mod tests {
    use super::{
        Reporter, State, lifecycle, read_state, startup_timeout, stop_in_directory, write_state,
    };
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn stop_requires_a_clean_final_state() {
        for outcome in ["stopped", "failed", "missing", "unfinished"] {
            let directory =
                std::env::temp_dir().join(format!("herdr-stop-{:032x}", rand::random::<u128>()));
            fs::create_dir(&directory).unwrap();
            write_state(
                &directory,
                &State {
                    pid: std::process::id(),
                    tunnel: "test".into(),
                    port: 1234,
                    session: "test".into(),
                    ready: true,
                    restarts: 0,
                    message: "ready".into(),
                },
            )
            .unwrap();
            let lock = lifecycle::try_lock(&directory.join("worker.lock"))
                .unwrap()
                .unwrap();
            let worker_directory = directory.clone();
            let worker = thread::spawn(move || {
                let _lock = lock;
                let started = Instant::now();
                while !worker_directory.join("stop").exists() {
                    assert!(started.elapsed() < Duration::from_secs(10));
                    thread::sleep(Duration::from_millis(10));
                }
                let reporter = Reporter {
                    directory: Some(worker_directory.clone()),
                };
                match outcome {
                    "stopped" => reporter.finish(&Ok(())).unwrap(),
                    "failed" => reporter
                        .finish(&Err(anyhow::anyhow!("child cleanup failed")))
                        .unwrap(),
                    "missing" => fs::remove_file(worker_directory.join("state.json")).unwrap(),
                    "unfinished" => {}
                    _ => unreachable!(),
                }
            });
            let result = stop_in_directory("test", &directory);
            worker.join().unwrap();
            match outcome {
                "stopped" => assert!(result.is_ok()),
                "failed" => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("child cleanup failed"))
                }
                "missing" => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("without a final status"))
                }
                "unfinished" => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("did not stop cleanly"))
                }
                _ => unreachable!(),
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn stop_ignores_saved_failures_when_not_running() {
        let directory = std::env::temp_dir().join(format!(
            "herdr-stop-idle-{:032x}",
            rand::random::<u128>()
        ));
        fs::create_dir(&directory).unwrap();
        assert!(stop_in_directory("test", &directory).is_ok());
        write_state(
            &directory,
            &State {
                pid: std::process::id(),
                tunnel: "test".into(),
                port: 1234,
                session: "test".into(),
                ready: false,
                restarts: 0,
                message: "failed: earlier error".into(),
            },
        )
        .unwrap();
        assert!(stop_in_directory("test", &directory).is_ok());
        assert_eq!(
            read_state(&directory).unwrap().unwrap().message,
            "failed: earlier error"
        );
        assert!(!directory.join("stop").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn worker_result_keeps_serve_and_reporter_errors() {
        let directory = std::env::temp_dir().join(format!(
            "herdr-finish-{:032x}",
            rand::random::<u128>()
        ));
        fs::create_dir(&directory).unwrap();
        let reporter = Reporter {
            directory: Some(directory.clone()),
        };
        let result = Err(anyhow::anyhow!("child cleanup failed"));
        let status = reporter.finish(&result);
        let error = format!("{:#}", super::combine_results(result, status).unwrap_err());
        assert!(error.contains("child cleanup failed"));
        assert!(error.contains("Missing managed Herdr state"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_budget_covers_provisioning_readiness_and_retries() {
        for (timeout, cooldown, retries) in [(1, 0, 1), (45, 2, 1), (600, 30, 3)] {
            let expected =
                12 * timeout * (u64::from(retries) + 1) + (8 + cooldown) * u64::from(retries) + 60;
            assert_eq!(
                startup_timeout(timeout, cooldown, retries),
                Duration::from_secs(expected)
            );
        }
        assert_eq!(startup_timeout(45, 2, 0), startup_timeout(45, 2, 1));
    }

    #[test]
    fn startup_budget_saturates_without_overflow() {
        assert_eq!(
            startup_timeout(u64::MAX, 0, 1),
            Duration::from_secs(u64::MAX)
        );
        assert_eq!(
            startup_timeout(1, u64::MAX, u32::MAX),
            Duration::from_secs(u64::MAX)
        );
    }
}
