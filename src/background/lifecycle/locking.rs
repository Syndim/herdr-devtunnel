use anyhow::{Context, Result};
use std::fs::{File, TryLockError};
use std::path::Path;

pub(crate) fn try_lock(path: &Path) -> Result<Option<File>> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error).context("Failed to lock guard state"),
    }
}

pub(crate) fn management_lock(directory: &Path) -> Result<File> {
    try_lock(&directory.join("management.lock"))?
        .context("Another guard management command is running; retry shortly.")
}

pub(crate) fn running(directory: &Path) -> Result<bool> {
    Ok(try_lock(&directory.join("worker.lock"))?.is_none())
}
