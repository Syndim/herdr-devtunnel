use std::path::PathBuf;
use std::time::Duration;

use super::state::{State, read_state, write_state};
use crate::herdr::config::Connection;
use anyhow::Context;
use anyhow::Result;

pub(crate) struct Reporter {
    pub(crate) directory: Option<PathBuf>,
}

impl Reporter {
    pub(crate) fn foreground() -> Self {
        Self { directory: None }
    }

    pub(crate) fn update(
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

    pub(crate) async fn stop_requested(&self) -> Result<()> {
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

    pub(crate) fn finish(&self, result: &Result<()>) -> Result<()> {
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
