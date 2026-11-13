#![cfg(unix)]

use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
    tool: PathBuf,
    port: u16,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let candidate =
                std::env::temp_dir().join(format!("herdr-lookup-{}-{id}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).unwrap();
                    break candidate;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        };
        let tool = root.join("devtunnel");
        fs::write(
            &tool,
            r##"#!/bin/sh
printf '%s\n' "$*" >> "$LOOKUP_ROOT/commands"
case "$1" in
  user) printf 'Logged in as fixture\n' ;;
  show)
    if [ -f "$LOOKUP_ROOT/created" ]; then
      printf 'Tunnel not found in use2: fixture\n' >&2
      exit 2
    fi
    printf '%s' "$LOOKUP_STDOUT"
    printf '%s' "$LOOKUP_STDERR" >&2
    exit "$LOOKUP_CODE"
    ;;
  create)
    : > "$LOOKUP_ROOT/created"
    printf '{"tunnelId":"fixture","clusterId":"use2"}\n'
    ;;
  *) printf 'Unexpected command\n' >&2; exit 1 ;;
esac
"##,
        )
        .unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Self { root, tool, port }
    }

    fn run(&self, tunnel: &str, code: i32, stdout: &str, stderr: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hd"))
            .args([
                "herdr",
                "server",
                "--foreground",
                "--tunnel",
                tunnel,
                "--port",
            ])
            .arg(self.port.to_string())
            .args(["--timeout", "5", "--hc"])
            .arg(&self.tool)
            .arg("--devtunnel")
            .arg(&self.tool)
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("LOOKUP_ROOT", &self.root)
            .env("LOOKUP_CODE", code.to_string())
            .env("LOOKUP_STDOUT", stdout)
            .env("LOOKUP_STDERR", stderr)
            .output()
            .unwrap()
    }

    fn commands(&self) -> String {
        fs::read_to_string(self.root.join("commands")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn exact_not_found_diagnostics_create_then_recheck() {
    for (tunnel, message, create) in [
        (
            "fixture",
            "Tunnel not found: fixture\n",
            "create fixture --json",
        ),
        (
            "fixture",
            "Tunnel not found: fixture\r\n",
            "create fixture --json",
        ),
        (
            "fixture",
            "Tunnel not found: fixture",
            "create fixture --json",
        ),
        (
            "fixture.use2",
            "Tunnel not found in use2: fixture\n",
            "create fixture --service-uri https://use2.rel.tunnels.api.visualstudio.com --json",
        ),
        (
            "fixture.USE2",
            "Tunnel not found in USE2: fixture\r\n",
            "create fixture --service-uri https://use2.rel.tunnels.api.visualstudio.com --json",
        ),
        (
            "a.use2",
            "Tunnel not found in use2: a\n",
            "create a --service-uri https://use2.rel.tunnels.api.visualstudio.com --json",
        ),
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(tunnel, 2, "", message);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Created tunnel was not found"),
            "{output:?}"
        );
        let commands = fixture.commands();
        assert!(commands.lines().any(|line| line == create), "{commands}");
        assert!(
            commands
                .lines()
                .any(|line| line == "show fixture.use2 --json"),
            "{commands}"
        );
        assert!(
            !commands
                .lines()
                .any(|line| line.starts_with("port ") || line.starts_with("host ")),
            "{commands}"
        );
    }
}

#[test]
fn lookup_errors_never_create_or_publish() {
    for (code, stdout, stderr) in [
        (1, "", "Tunnel not found: fixture\n"),
        (0, "", "Tunnel not found: fixture\n"),
        (3, "", "Tunnel not found: fixture\n"),
        (2, "unexpected output", "Tunnel not found: fixture\n"),
        (2, "", "Authentication failed\n"),
        (2, "", "Connection timed out\n"),
        (
            2,
            "",
            "Tunnel ID fixture prefix was not unique, shared by 2 tunnels\n",
        ),
        (2, "", "Tunnel not found: \n"),
        (2, "", "Tunnel not found: other\n"),
        (2, "", "Tunnel not found in use2: fixture\n"),
        (2, "", "Tunnel not found in fixture region\n"),
        (2, "", "Tunnel not found: fixture: access denied\n"),
        (2, "", "Tunnel not found: fixture\nAuthentication failed\n"),
        (2, "", "Authentication failed\nTunnel not found: fixture\n"),
        (2, "", " Tunnel not found: fixture\n"),
        (2, "", "Tunnel not found: fixture in use2 domain\n"),
        (2, "", "Tunnel not found: fixture.extra\n"),
        (2, "", "Tunnel not found: fixture\n\n"),
        (1, "", "Tunnel not found in use2: fixture\n"),
    ] {
        let fixture = Fixture::new();
        let output = fixture.run("fixture", code, stdout, stderr);
        assert!(!output.status.success(), "{code} {stdout:?} {stderr:?}");
        let commands = fixture.commands();
        assert!(
            !commands.lines().any(|line| line.starts_with("create ")
                || line.starts_with("port ")
                || line.starts_with("host ")),
            "{code} {stdout:?} {stderr:?}: {commands}"
        );
    }
}

#[test]
fn clustered_lookup_errors_never_create_or_publish() {
    for (tunnel, code, stdout, stderr) in [
        ("fixture.use2", 1, "", "Tunnel not found in use2: fixture\n"),
        ("fixture.use2", 0, "", "Tunnel not found in use2: fixture\n"),
        (
            "fixture.use2",
            2,
            "output",
            "Tunnel not found in use2: fixture\n",
        ),
        ("fixture.use2", 2, "", "Tunnel not found in "),
        (
            "fixture.use2",
            2,
            "",
            "Tunnel not found in fixture region\n",
        ),
        ("fixture.use2", 2, "", "Tunnel not found in use2: other\n"),
        ("fixture.use2", 2, "", "Tunnel not found in inc1: fixture\n"),
        ("fixture.use2", 2, "", "Tunnel not found in USE2: fixture\n"),
        ("fixture.use2", 2, "", "Tunnel not found: fixture\n"),
        ("fixture.use2", 2, "", "Tunnel not found: fixture.use2\n"),
        (
            "fixture.use2",
            2,
            "",
            "Tunnel not found in use2: fixture: access denied\n",
        ),
        (
            "fixture.use2",
            2,
            "",
            "Tunnel not found in use2: fixture\nAuthentication failed\n",
        ),
        (
            "fixture.use2",
            2,
            "",
            "Authentication failed\nTunnel not found in use2: fixture\n",
        ),
        (
            "fixture.use2",
            2,
            "",
            "Tunnel not found in use2: fixture\n\n",
        ),
        (
            "fixture.use2",
            2,
            "",
            " Tunnel not found in use2: fixture\n",
        ),
        (
            "fixture.extra.use2",
            2,
            "",
            "Tunnel not found in use2: fixture.extra\n",
        ),
        (
            "fixture.extra.use2",
            2,
            "",
            "Tunnel not found: fixture in extra.use2 domain\n",
        ),
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(tunnel, code, stdout, stderr);
        assert!(
            !output.status.success(),
            "{tunnel} {code} {stdout:?} {stderr:?}"
        );
        let commands = fixture.commands();
        assert!(
            !commands.lines().any(|line| line.starts_with("create ")
                || line.starts_with("port ")
                || line.starts_with("host ")),
            "{tunnel} {code} {stdout:?} {stderr:?}: {commands}"
        );
    }
}
