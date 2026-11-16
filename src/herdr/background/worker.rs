use std::fs;

use anyhow::{Context, Result, ensure};

use crate::config::ServerArgs;
use crate::herdr::server::serve;
use crate::process::combine_results;

use super::WorkerArgs;
use super::lifecycle;
use super::state::Reporter;

pub(crate) async fn worker(args: WorkerArgs) -> Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    ensure!(
        executable.parent() == Some(args.directory.as_path())
            && executable
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("worker-")),
        "server-worker must run from a private executable snapshot; use hd server"
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
