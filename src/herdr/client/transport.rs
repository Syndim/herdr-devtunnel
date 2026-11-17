use crate::herdr::config::Connection;
#[cfg(not(windows))]
use crate::herdr::validation::shell_quote;
use crate::herdr::validation::via_path;
#[cfg(windows)]
use crate::herdr::validation::windows_quote;
use anyhow::Result;

pub(super) fn via_command(c: &Connection, dt: String) -> Result<String> {
    let exe = via_path(std::env::current_exe()?.canonicalize()?)?;
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
