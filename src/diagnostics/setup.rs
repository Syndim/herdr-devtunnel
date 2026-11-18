use super::doctor::doctor;
use crate::herdr::config::{Role, SetupArgs};
use crate::herdr::validation::executable;
use anyhow::{Result, ensure};
use tokio::process::Command;

pub(crate) async fn setup(args: SetupArgs) -> Result<()> {
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
            println!("Server: hd server start --tunnel UNIQUE-NAME");
            println!("The server creates a missing tunnel/port or reuses a valid private tunnel.");
            println!("It runs in the background and returns only after tunnel readiness.");
            println!(
                "Manage: hd server status; hd server stop (add --name PROFILE for named servers)."
            );
        }
        if role != Role::Server {
            println!("Client: hd client connect --name remote --tunnel NAME.CLUSTER");
            println!("Verify: hc machine ls; hc exec --on remote whoami");
            println!("Use Herdr App with this local core; do not add the machine through SSH.");
        }
        println!("No dependencies, services, tunnels or machines are installed by setup.");
    }
    Ok(())
}
