use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("herdr-hook-test-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test home: {error}"),
            }
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn hook(input: &str, in_herdr: bool) -> Output {
    hook_event("agentStop", input, in_herdr)
}

fn hook_event(event: &str, input: &str, in_herdr: bool) -> Output {
    let home = Home::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd"));
    command
        .args(["herdr", "copilot-status", event])
        .env("HOME", &home.0)
        .env("USERPROFILE", &home.0)
        .current_dir(&home.0)
        .env_remove("HC_TERMINAL_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if in_herdr {
        command.env("HC_TERMINAL_ID", "test");
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn outside_herdr_hook_is_a_silent_noop() {
    let output = hook("not JSON", false);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn child_event_does_not_open_or_write_a_terminal() {
    let output = hook(r#"{"agentId":"child"}"#, true);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn permission_check_does_not_open_or_write_a_terminal() {
    let output = hook_event(
        "permissionRequest",
        r#"{"toolName":"bash","toolInput":{"command":"cargo test"}}"#,
        true,
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn foreground_stop_does_not_open_or_write_a_terminal() {
    for (event, input) in [
        ("agentStop", r#"{"stopReason":"end_turn"}"#),
        ("sessionEnd", r#"{"reason":"complete"}"#),
    ] {
        let output = hook_event(event, input, true);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn malformed_hook_input_is_reported() {
    let output = hook("not JSON", true);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Read Copilot hook JSON"));
}
