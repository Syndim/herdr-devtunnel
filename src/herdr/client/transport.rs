use crate::herdr::config::Connection;
#[cfg(not(windows))]
use crate::herdr::validation::shell_quote;
use crate::herdr::validation::via_path;
#[cfg(windows)]
use crate::herdr::validation::windows_quote;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

fn registration_executable(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let resolved = which::which(path)
            .with_context(|| format!("Executable not found: {}", path.display()))?;
        #[cfg(windows)]
        anyhow::ensure!(
            resolved
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
            "hc --via requires a native .exe launcher on Windows: {}",
            resolved.display()
        );
        return Ok(std::path::absolute(resolved)?);
    }

    let current = std::env::current_exe()?;
    if let Some(invoked) = std::env::args_os().next()
        && let Ok(path) = which::which(invoked)
        && let (Ok(candidate), Ok(actual)) = (path.canonicalize(), current.canonicalize())
        && candidate == actual
    {
        return Ok(std::path::absolute(path)?);
    }
    Ok(current)
}

pub(super) fn via_command(c: &Connection, dt: String, hd_path: Option<&Path>) -> Result<String> {
    let exe = via_path(registration_executable(hd_path)?)?;
    let via = [
        exe,
        "client".into(),
        "transport".into(),
        "--tunnel".into(),
        c.tunnel.clone(),
        "--port".into(),
        c.port.to_string(),
        "--session".into(),
        c.session.clone(),
        "--timeout".into(),
        c.timeout.to_string(),
        "--devtunnel".into(),
        dt,
    ]
    .iter()
    .map(|arg| {
        #[cfg(windows)]
        let quoted = windows_quote(arg);
        #[cfg(not(windows))]
        let quoted = shell_quote(arg);
        quoted
    })
    .collect::<Vec<_>>()
    .join(" ");
    Ok(via)
}
