use std::fs;
use std::io::{self, Write};
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
    run_hook(event, input, in_herdr, false)
}

fn run_hook(event: &str, input: &str, in_herdr: bool, legacy: bool) -> Output {
    let home = Home::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd"));
    if legacy {
        command.arg("herdr");
    }
    command
        .args(["copilot-status", event])
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
    write_hook_input(child.stdin.take().unwrap(), input, in_herdr).unwrap();
    child.wait_with_output().unwrap()
}

fn write_hook_input(mut stdin: impl Write, input: &str, in_herdr: bool) -> io::Result<()> {
    match stdin.write_all(input.as_bytes()) {
        Err(error) if !in_herdr && error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

#[test]
fn outside_herdr_hook_is_a_silent_noop() {
    let output = hook("not JSON", false);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn outside_herdr_hook_with_large_input_is_repeatedly_a_silent_noop() {
    let input = "x".repeat(2 * 1024 * 1024);
    for legacy in [false, true] {
        for _ in 0..8 {
            let output = run_hook("agentStop", &input, false, legacy);
            assert!(output.status.success(), "{output:?}");
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn hook_input_write_only_ignores_broken_pipe_outside_herdr() {
    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    for in_herdr in [false, true] {
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::PermissionDenied] {
            let result = write_hook_input(FailingWriter(kind), "input", in_herdr);
            if !in_herdr && kind == io::ErrorKind::BrokenPipe {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err().kind(), kind);
            }
        }
    }
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
fn legacy_copilot_status_hook_remains_compatible() {
    let output = run_hook("agentStop", "not JSON", false, true);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let output = run_hook("agentStop", r#"{"agentId":"child"}"#, true, true);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let output = run_hook("agentStop", "not JSON", true, true);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Read Copilot hook JSON"));
}

#[test]
fn malformed_hook_input_is_reported() {
    let output = hook("not JSON", true);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Read Copilot hook JSON"));
}
