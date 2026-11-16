use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::lifecycle;

#[derive(Serialize, Deserialize)]
pub(crate) struct State {
    pub(crate) pid: u32,
    pub(crate) tunnel: String,
    pub(crate) port: u16,
    pub(crate) session: String,
    pub(crate) ready: bool,
    pub(crate) restarts: u32,
    pub(crate) message: String,
}

pub(crate) fn runtime_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    let home = home
        .filter(|value| !value.is_empty())
        .context("Cannot locate the user's home directory")?;
    Ok(PathBuf::from(home).join(".herdr-devtunnel"))
}

pub(crate) fn directory(name: &str) -> Result<PathBuf> {
    let directory = runtime_directory()?.join(format!("server-{}", name.to_ascii_lowercase()));
    lifecycle::create_directory(&directory)?;
    directory
        .canonicalize()
        .context("Failed to resolve Herdr server directory")
}

pub(crate) fn write_state(directory: &Path, state: &State) -> Result<()> {
    let temporary = directory.join("state.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)
        .context("Failed to write Herdr state")?;
    fs::rename(temporary, directory.join("state.json")).context("Failed to publish Herdr state")
}

pub(crate) fn read_state(directory: &Path) -> Result<Option<State>> {
    let bytes = match fs::read(directory.join("state.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Failed to read Herdr state"),
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("Invalid Herdr state")?,
    ))
}
