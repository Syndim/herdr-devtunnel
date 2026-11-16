use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

pub(super) fn command(path: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(path);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
}

pub(super) async fn run_output(
    path: &PathBuf,
    args: &[&str],
    seconds: u64,
) -> Result<std::process::Output> {
    timeout(
        Duration::from_secs(seconds),
        command(path).args(args).stdin(Stdio::null()).output(),
    )
    .await
    .with_context(|| format!("{} timed out", path.display()))?
    .with_context(|| format!("Failed to run {}", path.display()))
}

pub(super) fn checked_output(
    path: &Path,
    args: &[&str],
    result: std::process::Output,
) -> Result<String> {
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

pub(super) async fn output(path: &PathBuf, args: &[&str], seconds: u64) -> Result<String> {
    checked_output(path, args, run_output(path, args, seconds).await?)
}

pub(super) async fn lookup_tunnel(
    path: &PathBuf,
    tunnel: &str,
    seconds: u64,
) -> Result<Option<Value>> {
    let args = ["show", tunnel, "--json"];
    let result = run_output(path, &args, seconds).await?;
    if crate::dt::tunnel_not_found(&result, tunnel) {
        return Ok(None);
    }
    let detail = checked_output(path, &args, result)?;
    Ok(Some(
        serde_json::from_str(&detail).context("Invalid tunnel details")?,
    ))
}
pub(super) fn combine_results(first: Result<()>, second: Result<()>) -> Result<()> {
    match (first, second) {
        (Err(first), Err(second)) => Err(first.context(format!("Additional error: {second:#}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
