use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::config::{ServerArgs, executable, parse_machine};
use crate::devtunnel::build_failure_matchers;

use super::SessionArgs;
use super::lifecycle;
use super::state::{State, directory, read_state, runtime_directory};

pub(crate) fn start(mut args: ServerArgs) -> Result<()> {
    build_failure_matchers(&args.patterns)?;
    args.name.make_ascii_lowercase();
    args.connection.tools.hc = executable(&args.connection.tools.hc)?.canonicalize()?;
    args.connection.tools.devtunnel =
        executable(&args.connection.tools.devtunnel)?.canonicalize()?;
    let directory = directory(&args.name)?;
    let management = lifecycle::management_lock(&directory)?;
    start_locked(args, &directory, management)
}

pub(crate) fn startup_timeout(timeout: u64, cooldown: u64, max_restarts: u32) -> Duration {
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
            "Server '{}' has a shutdown request pending; shutdown is cooperative and may take time. Check `hd status --name {}` and retry `hd stop --name {}` if needed; then retry `server`. See {}",
            args.name,
            args.name,
            args.name,
            directory.join("server.log").display()
        );
        let state = read_state(directory)?;
        let Some(state) = state.filter(|state| state.ready) else {
            bail!(
                "Server '{}' is running but not ready (it may still be starting or recovering). It remains active and may retry in the background. Check `hd status --name {}` and retry `server` when ready, or request shutdown with `hd stop --name {}`. See {}",
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
        .args(["server-worker", "--directory"])
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
                    "CLIENT COMMAND: hd client --name remote --tunnel {} --port {} --session {}",
                    state.tunnel, state.port, state.session
                );
                println!("Log: {}", log_path.display());
                return Ok(());
            }
        }
        if started.elapsed() >= startup_timeout {
            fs::write(directory.join("stop"), b"").context("Failed to cancel Herdr startup")?;
            bail!(
                "Herdr startup timed out; shutdown was requested but is not confirmed. Shutdown is cooperative and may take time. Check `hd status --name {}` and retry `hd stop --name {}` if needed. See {}",
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

pub(crate) fn status(args: SessionArgs) -> Result<()> {
    let directory = directory(&args.name)?;
    let _management = lifecycle::management_lock(&directory)?;
    show_status(&args.name, &directory)
}

pub(crate) fn stop(args: SessionArgs) -> Result<()> {
    let directory = directory(&args.name)?;
    let _management = lifecycle::management_lock(&directory)?;
    stop_in_directory(&args.name, &directory)
}

pub(crate) fn stop_in_directory(name: &str, directory: &Path) -> Result<()> {
    if !lifecycle::running(directory)? {
        println!("Server '{name}' is not running.");
        return Ok(());
    }
    fs::write(directory.join("stop"), b"").context("Failed to request Herdr shutdown")?;
    let started = Instant::now();
    while lifecycle::running(directory)? {
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Server '{}' has not stopped after 10 seconds; shutdown remains requested but is not confirmed. Shutdown is cooperative and may take time. Check `hd status --name {}` and retry `hd stop --name {}` if needed. See {}",
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

pub(crate) fn reconnect_all() -> Result<()> {
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
