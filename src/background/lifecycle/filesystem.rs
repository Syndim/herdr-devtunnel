use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub(crate) fn create_managed_directory(directory: &Path, private: bool) -> Result<()> {
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
pub(crate) fn validate_home(home: &Path) -> Result<()> {
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
    create_runtime_directory(runtime)?;
    create_managed_directory(directory, false)
}

pub(crate) fn create_runtime_directory(runtime: &Path) -> Result<()> {
    let home = runtime.parent().context("Missing home directory")?;
    #[cfg(unix)]
    validate_home(home)?;
    #[cfg(not(unix))]
    let _ = home;
    create_managed_directory(runtime, true)
}
