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
    if legacy {
        run_command(&["copilot-status", event], input, in_herdr)
    } else {
        run_command(
            &["agent", "status", event, "--provider", "copilot"],
            input,
            in_herdr,
        )
    }
}

fn run_command(args: &[&str], input: &str, in_herdr: bool) -> Output {
    let home = Home::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd"));
    command
        .args(args)
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
fn copilot_status_hook_remains_compatible() {
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

#[test]
fn legacy_root_hook_remains_compatible() {
    let output = run_command(&["copilot-status", "agentStop"], "not JSON", true);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Read Copilot hook JSON"));
}

#[test]
fn unknown_caller_is_a_silent_noop() {
    for in_herdr in [false, true] {
        let output = run_command(&["agent", "status", "agentStop"], "not JSON", in_herdr);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn copilot_caller_fixture() {
    if std::env::var_os("HD_AGENT_CALLER_FIXTURE").is_none() {
        return;
    }
    let output = run_command(&["agent", "status", "agentStop"], "not JSON", true);
    assert!(
        !output.status.success(),
        "Copilot ancestor was not detected: {output:?}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Read Copilot hook JSON"));
    // Also cover an intervening shell on Unix.
    #[cfg(unix)]
    {
        let output = Command::new("sh")
            .args([
                "-c",
                "printf 'not JSON' | \"$1\" agent status agentStop",
                "sh",
                env!("CARGO_BIN_EXE_hd"),
            ])
            .env("HC_TERMINAL_ID", "test")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Read Copilot hook JSON"));
    }
}

#[test]
fn detects_copilot_ancestor() {
    let home = Home::new();
    let fixture = home.0.join(if cfg!(windows) {
        "copilot.exe"
    } else {
        "copilot"
    });
    fs::copy(std::env::current_exe().unwrap(), &fixture).unwrap();
    let output = run_copied_executable(
        Command::new(fixture)
            .args(["--exact", "copilot_caller_fixture", "--nocapture"])
            .env("HD_AGENT_CALLER_FIXTURE", "1"),
    );
    assert!(output.status.success(), "{output:?}");
}

fn run_copied_executable(command: &mut Command) -> Output {
    // Parallel spawns can briefly inherit a just-copied executable's write
    // descriptor before exec closes it. Retry only that transient OS error.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match command.output() {
            Ok(output) => return output,
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => panic!("launch copied executable: {error}"),
        }
    }
}

fn integrate(home: &Home, copilot_home: Option<&PathBuf>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hd"));
    command
        .args(["agent", "integrate", "--provider", "copilot"])
        .env("HOME", &home.0)
        .env("USERPROFILE", &home.0)
        .env_remove("COPILOT_HOME");
    if let Some(path) = copilot_home {
        command.env("COPILOT_HOME", path);
    }
    command.output().unwrap()
}

#[test]
fn installs_user_hooks_without_changing_other_configuration() {
    let home = Home::new();
    let copilot = home.0.join(".copilot");
    fs::create_dir_all(copilot.join("hooks")).unwrap();
    fs::write(copilot.join("settings.json"), "existing settings").unwrap();
    fs::write(copilot.join("hooks/other.json"), "other hooks").unwrap();
    let output = integrate(&home, None);
    assert!(output.status.success(), "{output:?}");
    let path = copilot.join("hooks/herdr-devtunnel.json");
    let contents = fs::read(&path).unwrap();
    let config: serde_json::Value = serde_json::from_slice(&contents).unwrap();
    assert_eq!(config["version"], 1);
    let hooks = config["hooks"].as_object().unwrap();
    assert_eq!(hooks.len(), 9);
    assert!(!hooks.contains_key("sessionIdle"));
    for (event, entries) in hooks {
        let entries = entries.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            fs::canonicalize(entries[0]["exec"].as_str().unwrap()).unwrap(),
            fs::canonicalize(env!("CARGO_BIN_EXE_hd")).unwrap()
        );
        assert_eq!(
            entries[0]["args"],
            serde_json::json!(["agent", "status", event])
        );
        assert_eq!(entries[0]["type"], "command");
    }
    assert_eq!(
        fs::read_to_string(copilot.join("settings.json")).unwrap(),
        "existing settings"
    );
    assert_eq!(
        fs::read_to_string(copilot.join("hooks/other.json")).unwrap(),
        "other hooks"
    );
    assert!(integrate(&home, None).status.success());
    assert_eq!(fs::read(&path).unwrap(), contents);
}

#[test]
fn respects_copilot_home_and_paths_with_spaces() {
    let home = Home::new();
    let custom = home.0.join("custom copilot home");
    let output = integrate(&home, Some(&custom));
    assert!(output.status.success(), "{output:?}");
    assert!(custom.join("hooks/herdr-devtunnel.json").is_file());
    assert!(!home.0.join(".copilot").exists());
}

#[test]
fn installed_hook_handles_an_executable_path_with_spaces_and_unicode() {
    let home = Home::new();
    let directory = home.0.join("hd install ü");
    fs::create_dir(&directory).unwrap();
    let executable = directory.join(if cfg!(windows) { "hd.exe" } else { "hd" });
    fs::copy(env!("CARGO_BIN_EXE_hd"), &executable).unwrap();
    let output = run_copied_executable(
        Command::new(&executable)
            .args(["agent", "integrate", "--provider", "copilot"])
            .env("HOME", &home.0)
            .env("USERPROFILE", &home.0)
            .env_remove("COPILOT_HOME"),
    );
    assert!(output.status.success(), "{output:?}");
    let config: serde_json::Value = serde_json::from_slice(
        &fs::read(home.0.join(".copilot/hooks/herdr-devtunnel.json")).unwrap(),
    )
    .unwrap();
    let hook = &config["hooks"]["sessionStart"][0];
    let args: Vec<_> = hook["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let output = Command::new(hook["exec"].as_str().unwrap())
        .args(args)
        .env_remove("HC_TERMINAL_ID")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
}

#[test]
fn refuses_to_overwrite_an_existing_different_hook_file() {
    let home = Home::new();
    let path = home.0.join(".copilot/hooks/herdr-devtunnel.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "keep this").unwrap();
    let output = integrate(&home, None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Refusing to overwrite"));
    assert_eq!(fs::read_to_string(path).unwrap(), "keep this");
}

#[cfg(unix)]
#[test]
fn refuses_a_symlink_hook_file() {
    let home = Home::new();
    let target = home.0.join("target.json");
    fs::write(&target, "keep this").unwrap();
    let path = home.0.join(".copilot/hooks/herdr-devtunnel.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let output = integrate(&home, None);
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(target).unwrap(), "keep this");
}
