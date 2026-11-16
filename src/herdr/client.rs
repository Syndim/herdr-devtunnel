use anyhow::{Result, ensure};
use tokio::process::Command;

use super::ClientArgs;
#[cfg(not(windows))]
use crate::config::client_shell;
#[cfg(not(windows))]
use crate::config::shell_quote;
#[cfg(windows)]
use crate::config::windows_quote;
use crate::config::{executable, via_path};
#[cfg(windows)]
use crate::diagnostics::check_via_contract;
pub(super) async fn add(args: ClientArgs) -> Result<()> {
    let c = args.connection;
    let hc = executable(&c.tools.hc)?;
    let dt = via_path(executable(&c.tools.devtunnel)?)?;
    #[cfg(windows)]
    check_via_contract(&hc).await?;
    #[cfg(not(windows))]
    client_shell()?;
    let exe = via_path(std::env::current_exe()?.canonicalize()?)?;
    let via = [
        exe,
        "connect".into(),
        "--tunnel".into(),
        c.tunnel,
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
    eprintln!(
        "[herdr] Registering {} using a private tunnel, not SSH",
        args.name
    );
    let mut hc = Command::new(hc);
    if let Some(session) = &args.local_session {
        hc.env("HC_SESSION", session).env_remove("HC_SOCKET");
    }
    let status = hc
        .args([
            "machine",
            "add",
            "--name",
            &args.name,
            "--session",
            &c.session,
            "--via",
            &via,
        ])
        .status()
        .await?;
    ensure!(status.success(), "hc machine add failed: {status}");
    Ok(())
}
