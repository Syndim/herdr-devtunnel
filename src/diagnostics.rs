use std::path::PathBuf;

use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::json;
use tokio::process::Command;

#[cfg(not(windows))]
use crate::config::client_shell;
use crate::config::{DoctorArgs, SetupArgs};
use crate::config::{Role, executable};
use crate::process::output;
#[derive(Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

pub(super) async fn doctor(args: DoctorArgs) -> Result<()> {
    let mut checks = Vec::new();
    for (name, path) in [("hc", &args.tools.hc), ("devtunnel", &args.tools.devtunnel)] {
        let result = async {
            let path = executable(path)?;
            let version = output(&path, &["--version"], 45).await?;
            if name == "hc" {
                if args.role != Role::Server {
                    check_via_contract(&path).await?;
                }
                if args.role != Role::Client {
                    ensure!(
                        output(&path, &["connect", "--help"], 45)
                            .await?
                            .contains("--session"),
                        "hc connect must support --session"
                    );
                }
            }
            Ok(format!("{}; {}", version.trim(), path.display()))
        }
        .await;
        checks.push(check(name, result));
    }
    let login = async {
        let path = executable(&args.tools.devtunnel)?;
        let login = output(&path, &["user", "show"], 45).await?;
        ensure!(
            login.to_lowercase().contains("logged in as "),
            "Run devtunnel user login interactively"
        );
        Ok(login.trim().into())
    }
    .await;
    checks.push(check("devtunnel-login", login));
    #[cfg(not(windows))]
    if args.role != Role::Server {
        checks.push(check(
            "sh (required by hc --via)",
            client_shell().map(|p| p.display().to_string()),
        ));
    }
    let ok = checks.iter().all(|c| c.ok);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": ok, "platform": std::env::consts::OS, "checks": checks
            }))?
        );
    } else {
        for check in checks {
            println!(
                "{} {}: {}",
                if check.ok { "OK" } else { "FAIL" },
                check.name,
                check.detail
            );
        }
    }
    ensure!(
        ok,
        "Herdr prerequisites are not ready; no tunnels or machine registrations were changed"
    );
    Ok(())
}

fn check(name: &'static str, result: Result<String>) -> Check {
    match result {
        Ok(detail) => Check {
            name,
            ok: true,
            detail,
        },
        Err(error) => Check {
            name,
            ok: false,
            detail: format!("{error:#}"),
        },
    }
}

pub(super) async fn setup(args: SetupArgs) -> Result<()> {
    if args.login {
        let status = Command::new(executable(&args.doctor.tools.devtunnel)?)
            .args(["user", "login"])
            .status()
            .await?;
        ensure!(status.success(), "Dev Tunnels login failed: {status}");
    }
    let role = args.doctor.role;
    let json = args.doctor.json;
    doctor(args.doctor).await?;
    if !json {
        println!("Use the SAME Dev Tunnels account and identity provider on both machines.");
        if role != Role::Client {
            println!("Server: hd server --tunnel UNIQUE-NAME");
            println!("The server creates a missing tunnel/port or reuses a valid private tunnel.");
            println!("It runs in the background and returns only after tunnel readiness.");
            println!("Manage: hd status; hd stop (add --name PROFILE for named servers).");
        }
        if role != Role::Server {
            println!("Client: hd client --name remote --tunnel NAME.CLUSTER");
            println!("Verify: hc machine ls; hc exec --on remote whoami");
            println!("Use Herdr App with this local core; do not add the machine through SSH.");
        }
        println!("No dependencies, services, tunnels or machines are installed by setup.");
    }
    Ok(())
}

pub(super) async fn check_via_contract(hc: &PathBuf) -> Result<()> {
    let help = output(hc, &["machine", "add", "--help"], 45).await?;
    ensure!(help.contains("--via"), "hc machine add must support --via");
    #[cfg(windows)]
    ensure!(
        help.contains("CommandLineToArgvW") && help.contains("no shell"),
        "hc machine add --via must support Windows direct execution (CommandLineToArgvW, no shell)"
    );
    Ok(())
}
