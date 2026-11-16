use anyhow::{Context, Result};
use log::warn;
use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

pub(crate) fn sweep_snapshots(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory).context("Failed to enumerate old guard snapshots")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("worker-")
            && entry.file_type()?.is_file()
            && let Err(error) = fs::remove_file(entry.path())
        {
            warn!(
                "Could not remove old guard snapshot {}: {error}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

pub(crate) fn detach(command: &mut Command) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
        use windows::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };

        // Rust's Windows spawn inherits all inheritable handles, including the
        // launcher's original output pipes even when child stdio is redirected.
        // Keeping those pipes alive would make callers using output() hang.
        for kind in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle =
                unsafe { GetStdHandle(kind) }.context("Failed to get launcher stdio handle")?;
            if !handle.is_invalid() {
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
                    .context("Failed to prevent inheritance of launcher stdio")?;
            }
        }
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: independent of the caller's console.
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Do not receive signals sent to the invoking shell's foreground group.
        command.process_group(0);
    }
    Ok(())
}
