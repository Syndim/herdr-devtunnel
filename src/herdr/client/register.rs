use anyhow::{Result, ensure};
use tokio::process::Command;

#[cfg(windows)]
use crate::diagnostics::check_via_contract;
use crate::herdr::config::ClientArgs;
#[cfg(not(windows))]
use crate::herdr::validation::client_shell;
use crate::herdr::validation::{executable, via_path};
pub(crate) async fn add(args: ClientArgs) -> Result<()> {
    let c = args.connection;
    let hc = executable(&c.tools.hc)?;
    let dt = via_path(executable(&c.tools.devtunnel)?)?;
    #[cfg(windows)]
    check_via_contract(&hc).await?;
    #[cfg(not(windows))]
    client_shell()?;
    let via = super::transport::via_command(&c, dt)?;
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
