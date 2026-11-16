use std::fs::{self, File, TryLockError};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use log::warn;

fn create_managed_directory(directory: &Path, private: bool) -> Result<()> {
    #[cfg(unix)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to create {}", directory.display()));
        }
    }
    let metadata = fs::symlink_metadata(directory)
        .with_context(|| format!("Failed to inspect {}", directory.display()))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "{} is not a directory or is a symbolic link",
        directory.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        anyhow::ensure!(
            metadata.uid() == unsafe { geteuid() },
            "{} is not owned by the current user",
            directory.display()
        );
        if private {
            anyhow::ensure!(
                metadata.mode() & 0o7777 == 0o700,
                "{} must have permissions 0700; inspect and secure it before retrying",
                directory.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}

#[cfg(unix)]
fn validate_home(home: &Path) -> Result<()> {
    use std::collections::VecDeque;
    use std::os::unix::fs::MetadataExt;

    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let uid = unsafe { geteuid() };
    let absolute = if home.is_absolute() {
        home.to_path_buf()
    } else {
        std::env::current_dir()?.join(home)
    };
    let mut pending: VecDeque<_> = absolute
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect();
    let mut resolved = std::path::PathBuf::from("/");
    let mut links = 0;
    let check_ancestor = |path: &Path, metadata: &fs::Metadata| -> Result<()> {
        anyhow::ensure!(metadata.is_dir(), "{} is not a directory", path.display());
        anyhow::ensure!(
            metadata.uid() == uid || metadata.uid() == 0,
            "{} is not owned by the current user or root",
            path.display()
        );
        anyhow::ensure!(
            metadata.mode() & 0o022 == 0 || (metadata.uid() == 0 && metadata.mode() & 0o1000 != 0),
            "{} is a writable home ancestor",
            path.display()
        );
        Ok(())
    };
    let root = fs::symlink_metadata(&resolved).context("Failed to inspect filesystem root")?;
    check_ancestor(&resolved, &root)?;
    while let Some(component) = pending.pop_front() {
        if component == "/" {
            resolved = "/".into();
        } else if component == ".." {
            resolved.pop();
        } else {
            let next = resolved.join(component);
            let metadata = fs::symlink_metadata(&next)
                .with_context(|| format!("Failed to inspect {}", next.display()))?;
            if metadata.file_type().is_symlink() {
                anyhow::ensure!(
                    metadata.uid() == uid || metadata.uid() == 0,
                    "{} is a symbolic link not owned by the current user or root",
                    next.display()
                );
                links += 1;
                anyhow::ensure!(
                    links <= 40,
                    "Too many symbolic links resolving {}",
                    home.display()
                );
                let target = fs::read_link(&next)
                    .with_context(|| format!("Failed to resolve {}", next.display()))?;
                for component in target.components().rev() {
                    pending.push_front(component.as_os_str().to_os_string());
                }
            } else {
                if !pending.is_empty() {
                    check_ancestor(&next, &metadata)?;
                }
                resolved = next;
            }
        }
    }
    let metadata = fs::symlink_metadata(&resolved)
        .with_context(|| format!("Failed to inspect home {}", home.display()))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "{} is not a home directory",
        home.display()
    );
    anyhow::ensure!(
        metadata.uid() == uid,
        "Home {} is not owned by the current user",
        home.display()
    );
    anyhow::ensure!(
        metadata.mode() & 0o022 == 0,
        "Home {} must not be group or world writable",
        home.display()
    );
    Ok(())
}

pub(crate) fn create_directory(directory: &Path) -> Result<()> {
    let runtime = directory
        .parent()
        .context("Missing guard runtime directory")?;
    let home = runtime.parent().context("Missing home directory")?;
    #[cfg(unix)]
    validate_home(home)?;
    #[cfg(not(unix))]
    let _ = home;
    create_managed_directory(runtime, true)?;
    create_managed_directory(directory, false)
}

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
