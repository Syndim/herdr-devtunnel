use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::lifecycle;

const START_DEFAULTS_FILE: &str = "start-defaults.json";

#[derive(Serialize, Deserialize)]
pub(crate) struct StartDefaults {
    pub(crate) tunnel: String,
    pub(crate) name: String,
    pub(crate) port: u16,
}

impl StartDefaults {
    pub(crate) fn load() -> Result<Option<Self>> {
        let runtime = runtime_directory()?;
        match fs::symlink_metadata(&runtime) {
            Ok(_) => (),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Failed to inspect Herdr runtime directory"),
        }
        lifecycle::create_runtime_directory(&runtime)?;
        let path = runtime.join(START_DEFAULTS_FILE);
        let bytes = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file(),
                    "{} is not a regular file",
                    path.display()
                );
                let mut file = fs::File::open(&path)
                    .with_context(|| format!("Failed to open {}", path.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let metadata = file.metadata()?;
                    let owner = fs::metadata(&runtime)?.uid();
                    ensure!(
                        metadata.uid() == owner && metadata.mode() & 0o022 == 0,
                        "{} must be owned by the current user and not writable by others",
                        path.display()
                    );
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                bytes
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to inspect {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("Invalid saved server start settings in {}", path.display()))
    }

    pub(super) fn save(name: &str, tunnel: &str, port: u16) {
        let saved = Self {
            tunnel: tunnel.into(),
            name: name.to_ascii_lowercase(),
            port,
        };
        if let Err(error) = saved.publish() {
            eprintln!("[herdr] Could not save server start settings: {error:#}");
        }
    }

    fn publish(&self) -> Result<()> {
        let runtime = runtime_directory()?;
        lifecycle::create_runtime_directory(&runtime)?;
        let bytes = serde_json::to_vec_pretty(self)?;
        let temporary = runtime.join(format!(
            "start-defaults-{:032x}.tmp",
            rand::random::<u128>()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .context("Failed to create start settings snapshot")?;
        let written = file.write_all(&bytes);
        drop(file);
        let result =
            written.and_then(|()| fs::rename(&temporary, runtime.join(START_DEFAULTS_FILE)));
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result.context("Failed to publish server start settings")
    }
}

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
