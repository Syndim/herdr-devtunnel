use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    cli: PathBuf,
    hc: PathBuf,
    dt: PathBuf,
    port: u16,
}

impl Fixture {
    fn new() -> Self {
        // create_dir is atomic: parallel tests and stale directories cannot share state.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = loop {
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let candidate = std::env::temp_dir().join(format!(
                "herdr-devtunnel's 測試 🦀 {}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    #[cfg(unix)]
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).unwrap();
                    break candidate;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        };
        let suffix = std::env::consts::EXE_SUFFIX;
        let cli = root.join(format!("hd{suffix}"));
        let hc = root.join(format!("hc{suffix}"));
        let dt = root.join(format!("devtunnel{suffix}"));
        fs::copy(env!("CARGO_BIN_EXE_hd"), &cli).unwrap();
        let result = Command::new("rustc")
            .arg("--edition=2024")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/herdr.rs"))
            .arg("-o")
            .arg(&hc)
            .output()
            .unwrap();
        success(&result);
        fs::copy(&hc, &dt).unwrap();
        let port = allocate_port();
        Self {
            root,
            cli,
            hc,
            dt,
            port,
        }
    }

    fn command(&self, subcommand: &str) -> Command {
        self.command_with_devtunnel(subcommand, &self.dt)
    }

    fn command_with_devtunnel(&self, subcommand: &str, devtunnel: &Path) -> Command {
        let mut command = Command::new(&self.cli);
        command.args(["herdr", subcommand]);
        if !matches!(subcommand, "status" | "stop" | "reconnect" | "r") {
            command
                .arg("--hc")
                .arg(&self.hc)
                .arg("--devtunnel")
                .arg(devtunnel);
        }
        if matches!(subcommand, "reconnect" | "r") {
            command
                .arg("--hc")
                .arg(&self.hc)
                .env("HERDR_TEST_FIXTURE_API", "1");
        }
        for key in [
            "HERDR_TEST_FIXTURE_LOOKUP_ERROR",
            "HERDR_TEST_FIXTURE_WRONG_PORT",
            "HERDR_TEST_FIXTURE_SHARED",
            "HERDR_TEST_FIXTURE_CREATED_CLUSTER",
            "HERDR_TEST_FIXTURE_EARLY_CLOSE",
            "HERDR_TEST_FIXTURE_HANG_AFTER_EOF",
            "HERDR_TEST_FIXTURE_HC_GUARD",
            "HERDR_TEST_FIXTURE_HOST_MODE",
            "HERDR_TEST_FIXTURE_LEGACY_HC",
            "HERDR_TEST_FIXTURE_REQUIRE_NO_HC_SOCKET",
        ] {
            command.env_remove(key);
        }
        if !matches!(subcommand, "reconnect" | "r") {
            command.env_remove("HERDR_TEST_FIXTURE_API");
        }
        command
            .env("HERDR_TEST_FIXTURE_ROOT", &self.root)
            .env("HERDR_TEST_FIXTURE_PORT", self.port.to_string())
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .current_dir(&self.root);
        command
    }

    fn connection(&self, subcommand: &str) -> Command {
        let mut command = self.command(subcommand);
        command.args([
            "--tunnel",
            "fixture.use2",
            "--port",
            &self.port.to_string(),
            "--session",
            "work",
            "--timeout",
            "5",
        ]);
        command
    }

    fn foreground_server(&self) -> Command {
        let mut command = self.connection("server");
        command.arg("--foreground");
        command
    }

    fn state(&self) -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(
                self.root
                    .join(".herdr-devtunnel")
                    .join("server-default")
                    .join("state.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(self.root.join(".herdr-devtunnel")) {
            for entry in entries {
                let entry = entry.unwrap();
                let filename = entry.file_name();
                let Some(name) = filename.to_str().unwrap().strip_prefix("server-") else {
                    continue;
                };
                let output = self
                    .command("stop")
                    .args(["--name", name])
                    .output()
                    .unwrap();
                if !output.status.success() {
                    eprintln!(
                        "Failed to stop fixture server: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    return;
                }
            }
        }
        for _ in 0..50 {
            if fs::remove_dir_all(&self.root).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        eprintln!("Could not clean fixture {}", self.root.display());
    }
}

fn allocate_port() -> u16 {
    reserve_port().local_addr().unwrap().port()
}

fn reserve_port() -> TcpListener {
    static PORTS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    let mut ports = PORTS.lock().unwrap();
    loop {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if !ports.contains(&port) {
            ports.push(port);
            return listener;
        }
    }
}

struct HcGuard(PathBuf);

impl Drop for HcGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Running {
    child: Child,
    log: Arc<Mutex<String>>,
    stdout_reader: Option<thread::JoinHandle<Vec<u8>>>,
    stderr_reader: Option<thread::JoinHandle<()>>,
}

impl Running {
    fn start(mut command: Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stdout_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            BufReader::new(stdout).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let stderr = child.stderr.take().unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        let logs = log.clone();
        let stderr_reader = thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) => {
                        logs.lock().unwrap().push_str(&format!("{line}\n"));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            child,
            log,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
        }
    }

    fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.log.lock().unwrap().contains("CLIENT COMMAND:") {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "{}",
                self.log.lock().unwrap()
            );
            assert!(Instant::now() < deadline, "{}", self.log.lock().unwrap());
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn output(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "{}", self.log.lock().unwrap());
            thread::sleep(Duration::from_millis(25));
        };
        let stdout_reader = self.stdout_reader.take().unwrap();
        let stderr_reader = self.stderr_reader.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !stdout_reader.is_finished() || !stderr_reader.is_finished() {
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for child output: {}",
                self.log.lock().unwrap()
            );
            thread::sleep(Duration::from_millis(25));
        }
        let stdout = stdout_reader.join().unwrap();
        stderr_reader.join().unwrap();
        let stderr = self.log.lock().unwrap().as_bytes().to_vec();
        Output {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bootstrap() -> &'static str {
    "sh -c 'p=$(command -v herdr-core) || p=$HOME/.local/bin/herdr-core; [ -x \"$p\" ] || { echo \"herdr-core is not installed here\" >&2; exit 127; }; HC_SESSION=work exec \"$p\" connect'"
}

fn process_alive(pid: &str) -> bool {
    #[cfg(windows)]
    {
        let result = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "tasklist failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8_lossy(&result.stdout).contains(&format!("\"{pid}\""))
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

fn wait_for_exit(pid: &str) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while process_alive(pid) {
        if Instant::now() >= deadline {
            #[cfg(unix)]
            let details = Command::new("ps")
                .args(["-p", pid, "-o", "pid=,ppid=,stat=,lstart=,command="])
                .output()
                .map(|output| {
                    format!(
                        "stdout: {}\nstderr: {}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )
                })
                .unwrap_or_else(|error| format!("ps failed: {error}"));
            #[cfg(windows)]
            let details = "inspect tasklist for process details".to_string();
            panic!("Fixture child {pid} did not exit within 12 seconds: {details}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn binary_help_identifies_hd() {
    let output = Command::new(env!("CARGO_BIN_EXE_hd"))
        .arg("--help")
        .output()
        .unwrap();
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Usage: hd{} ", std::env::consts::EXE_SUFFIX)),
        "{stdout}"
    );
}

#[test]
fn cli_rejects_unsafe_identifiers_and_options_before_spawning_tools() {
    let fixture = Fixture::new();
    let invalid: &[&[&str]] = &[
        &["server"],
        &["client", "--tunnel", "fixture.use2"],
        &["server", "--tunnel", "fixture.use2", "--port", "0"],
        &[
            "server",
            "--tunnel",
            "fixture.use2",
            "--session",
            "../other",
        ],
        &["server", "--tunnel", "fixture.use2", "--name", "../other"],
        &["server", "--tunnel", "fixture.use2", "--allow-anonymous"],
        &["server", "--tunnel", "fixture.use2", "--create"],
        &["stop", "--name", "../other"],
    ];
    for args in invalid {
        let output = Command::new(&fixture.cli)
            .arg("herdr")
            .args(*args)
            .env("HOME", &fixture.root)
            .env("USERPROFILE", &fixture.root)
            .current_dir(&fixture.root)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
    }
    assert!(!fixture.root.join("tunnel-commands").exists());
    assert!(!fixture.root.join(".herdr-devtunnel").exists());
}

#[test]
fn client_local_session_only_changes_machine_add_environment() {
    let fixture = Fixture::new();
    let mut server = Running::start(fixture.foreground_server());
    server.ready();
    let socket = fixture.root.join("inherited.sock");
    let socket_value = format!("Some:{}", socket.display());

    for (local_session, inherited_session, inherited_socket, expected_session, expected_socket) in [
        (None, false, false, "None", "None"),
        (None, true, false, "Some:inherited", "None"),
        (None, false, true, "None", socket_value.as_str()),
        (None, true, true, "Some:inherited", socket_value.as_str()),
        (Some("local"), true, true, "Some:local", "None"),
        (Some("default"), true, true, "Some:default", "None"),
    ] {
        let mut command = fixture.connection("client");
        command.args(["--name", "workbox"]);
        if inherited_session {
            command.env("HC_SESSION", "inherited");
        } else {
            command.env_remove("HC_SESSION");
        }
        if inherited_socket {
            command.env("HC_SOCKET", &socket);
        } else {
            command.env_remove("HC_SOCKET");
        }
        if let Some(session) = local_session {
            command.args(["--local-session", session]);
        }
        success(&command.output().unwrap());
        assert_eq!(
            fs::read_to_string(fixture.root.join("machine-HC_SESSION")).unwrap(),
            expected_session
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("machine-HC_SOCKET")).unwrap(),
            expected_socket
        );
        let args = fs::read_to_string(fixture.root.join("machine-args")).unwrap();
        assert!(args.contains("--session\nwork\n--via\n"), "{args}");
        assert!(!args.contains("--local-session"), "{args}");
        #[cfg(not(windows))]
        assert!(args.contains("'--session' 'work'"), "{args}");
        #[cfg(windows)]
        {
            assert!(args.contains("\"--session\" \"work\""), "{args}");
            assert_eq!(
                fs::read_to_string(fixture.root.join("machine-help-HC_SESSION")).unwrap(),
                if inherited_session {
                    "Some:inherited"
                } else {
                    "None"
                }
            );
            assert_eq!(
                fs::read_to_string(fixture.root.join("machine-help-HC_SOCKET")).unwrap(),
                if inherited_socket {
                    socket_value.as_str()
                } else {
                    "None"
                }
            );
        }
    }
}

#[test]
#[cfg(windows)]
fn client_uses_direct_execution_without_git_or_shell() {
    let fixture = Fixture::new();
    let path = &fixture.root;
    assert!(!path.join("git.exe").exists());
    assert!(!path.join("sh.exe").exists());

    let doctor = fixture
        .command("doctor")
        .args(["--role", "client", "--json"])
        .env("PATH", path)
        .output()
        .unwrap();
    success(&doctor);
    let checks: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(checks["ok"], true);
    assert!(
        !checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["name"].as_str().unwrap().contains("sh ") })
    );

    let legacy_doctor = fixture
        .command("doctor")
        .args(["--role", "client", "--json"])
        .env("PATH", path)
        .env("HERDR_TEST_FIXTURE_LEGACY_HC", "1")
        .output()
        .unwrap();
    assert!(!legacy_doctor.status.success());
    let legacy_client = fixture
        .connection("client")
        .args(["--name", "workbox"])
        .env("PATH", path)
        .env("HERDR_TEST_FIXTURE_LEGACY_HC", "1")
        .output()
        .unwrap();
    assert!(!legacy_client.status.success());
    assert!(String::from_utf8_lossy(&legacy_client.stderr).contains("CommandLineToArgvW"));
    assert!(!fixture.root.join("machine-args").exists());

    let mut server = Running::start(fixture.foreground_server());
    server.ready();
    let result = fixture
        .connection("client")
        .args(["--name", "workbox"])
        .env("PATH", path)
        .output()
        .unwrap();
    success(&result);
    let child_path = fs::read_to_string(fixture.root.join("machine-path")).unwrap();
    assert_eq!(
        std::env::split_paths(&child_path).collect::<Vec<_>>(),
        vec![path.clone()]
    );
}

#[test]
fn native_transport_and_setup_use_only_scratch_processes() {
    let mut fixture = Fixture::new();
    let occupied = reserve_port();
    fixture.port = occupied.local_addr().unwrap().port();
    let doctor = fixture
        .command("setup")
        .args(["--role", "both", "--json"])
        .output()
        .unwrap();
    success(&doctor);
    let json: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert!(!fixture.root.join("tunnel-exists").exists());
    assert!(!fixture.root.join("machine-args").exists());
    let setup = fixture
        .command("setup")
        .args(["--role", "both"])
        .output()
        .unwrap();
    success(&setup);
    let setup_output = String::from_utf8_lossy(&setup.stdout);
    assert!(
        setup_output.contains("Server: hd herdr server --tunnel UNIQUE-NAME"),
        "{setup_output}"
    );
    assert!(
        setup_output.contains("Client: hd herdr client --name remote --tunnel NAME.CLUSTER"),
        "{setup_output}"
    );

    for bootstrap in [
        "sh -c 'echo arbitrary'".to_string(),
        format!("{}; echo unsafe", bootstrap()),
        bootstrap().replace("HC_SESSION=work", "HC_SESSION=other"),
        bootstrap().replace("HC_SESSION=work ", ""),
    ] {
        let invalid = fixture
            .connection("connect")
            .arg(&bootstrap)
            .output()
            .unwrap();
        assert!(!invalid.status.success(), "{bootstrap}");
        let stderr = String::from_utf8_lossy(&invalid.stderr);
        assert!(
            stderr.contains("Unsupported hc bootstrap")
                || stderr.contains("hc session does not match"),
            "{stderr}"
        );
        assert!(!fixture.root.join("connect-pid").exists());
    }

    let refused = fixture.foreground_server().output().unwrap();
    assert!(!refused.status.success());
    assert!(!fixture.root.join("tunnel-exists").exists());
    drop(occupied);

    let mut server_command = fixture.foreground_server();
    server_command
        .env("HC_SOCKET", fixture.root.join("wrong.sock"))
        .env("HERDR_TEST_FIXTURE_REQUIRE_NO_HC_SOCKET", "1");
    let mut server = Running::start(server_command);
    server.ready();
    assert!(
        fs::read_to_string(fixture.root.join("tunnel-commands"))
            .unwrap()
            .contains("create fixture --service-uri https://use2.rel.tunnels.api.visualstudio.com --json\n")
    );
    assert!(fixture.root.join("port-exists").exists());
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    for data in [
        b"GET / HTTP/1.1\r\n".to_vec(),
        b"{\"protocol\":\"wrong\",\"session\":\"work\"}\n".to_vec(),
        b"{\"protocol\":\"herdr-devtunnel/1\",\"session\":\"wrong\"}\n".to_vec(),
        b"{\"protocol\":\"herdr-devtunnel/1\"}\n".to_vec(),
        vec![b'x'; 4097],
    ] {
        let mut socket = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.write_all(&data).unwrap();
        let mut bytes = Vec::new();
        match socket.read_to_end(&mut bytes) {
            Ok(_) => {}
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ),
                "{error}"
            ),
        }
        assert!(bytes.is_empty());
    }
    assert!(!fixture.root.join("hc-connections").exists());
    // Fragmented handshake followed immediately by binary payload.
    let mut socket = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket.write_all(b"{\"protocol\":").unwrap();
    socket
        .write_all(b"\"herdr-devtunnel/1\",\"session\":\"work\"}\n\0\xffpayload")
        .unwrap();
    socket.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).unwrap();
    let newline = response.iter().position(|b| *b == b'\n').unwrap();
    assert_eq!(&response[newline + 1..], b"\0\xffpayload");

    let bytes: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 256) as u8).collect();
    let mut connector = fixture.connection("connect");
    connector.arg(bootstrap()).stdin(Stdio::piped());
    let mut connector = Running::start(connector);
    let mut input = connector.child.stdin.take().unwrap();
    let payload = bytes.clone();
    let writer = thread::spawn(move || input.write_all(&payload).unwrap());
    let result = connector.output();
    writer.join().unwrap();
    success(&result);
    assert_eq!(result.stdout, bytes);
    wait_for_exit(&fs::read_to_string(fixture.root.join("connect-pid")).unwrap());

    let registration = fixture
        .connection("client")
        .args(["--name", "scratch"])
        .output()
        .unwrap();
    success(&registration);
    let registration_args = fs::read_to_string(fixture.root.join("machine-args")).unwrap();
    let exe = fixture.cli.canonicalize().unwrap();
    #[cfg(not(windows))]
    let expected_via = format!(
        "--via\n'{}' 'herdr' 'connect'",
        exe.to_str().unwrap().replace('\'', "'\\''")
    );
    #[cfg(windows)]
    let expected_via = format!("--via\n\"{}\" \"herdr\" \"connect\"", exe.display());
    assert!(
        registration_args.contains(&expected_via),
        "{registration_args}"
    );
    wait_for_exit(&fs::read_to_string(fixture.root.join("connect-pid")).unwrap());

    let mut connector = fixture.connection("connect");
    connector.arg(bootstrap()).stdin(Stdio::piped());
    let _ = fs::remove_file(fixture.root.join("connect-pid"));
    let mut connector = Running::start(connector);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.root.join("connect-pid").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(25));
    }
    let pid = fs::read_to_string(fixture.root.join("connect-pid")).unwrap();
    connector.child.kill().unwrap();
    connector.child.wait().unwrap();
    wait_for_exit(&pid);
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    wait_for_exit(&host_pid);
    let before_reuse = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();

    let mut early_server = fixture.foreground_server();
    early_server.env("HERDR_TEST_FIXTURE_EARLY_CLOSE", "1");
    let mut early_server = Running::start(early_server);
    early_server.ready();
    let after_reuse = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(!after_reuse[before_reuse.len()..].contains("create "));
    let mut connector = fixture.connection("connect");
    connector.arg(bootstrap()).stdin(Stdio::piped());
    let result = Running::start(connector).output();
    success(&result);
    assert_eq!(result.stdout, b"remote-closed");
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    #[cfg(unix)]
    {
        assert!(
            Command::new("kill")
                .args(["-TERM", &early_server.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        success(&early_server.output());
    }
    #[cfg(windows)]
    {
        early_server.child.kill().unwrap();
        early_server.child.wait().unwrap();
    }
    wait_for_exit(&host_pid);
}

#[test]
fn server_reuses_or_repairs_only_a_valid_private_tunnel() {
    let fixture = Fixture::new();
    for error in ["auth", "network", "unknown", "wrong-code", "malformed"] {
        let output = fixture
            .foreground_server()
            .env("HERDR_TEST_FIXTURE_LOOKUP_ERROR", error)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{error}");
        assert!(!fixture.root.join("tunnel-exists").exists(), "{error}");
    }
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    for flag in ["HERDR_TEST_FIXTURE_SHARED", "HERDR_TEST_FIXTURE_WRONG_PORT"] {
        let output = fixture.foreground_server().env(flag, "1").output().unwrap();
        assert!(!output.status.success(), "{flag}");
        assert!(!fixture.root.join("port-exists").exists(), "{flag}");
    }
    let port = fixture.port;
    for metadata in [
        serde_json::json!({"tunnel": {"ports": [] , "accessControl": ["anonymous"]}}),
        serde_json::json!({"tunnel": {"ports": "invalid"}}),
        serde_json::json!({"tunnel": {"ports": [{"portNumber": port}, {"portNumber": 1}]}}),
        serde_json::json!({"tunnel": {"ports": [{"portNumber": port}], "accessControl": ["anonymous"]}}),
        serde_json::json!({"tunnel": {"ports": [{"portNumber": port, "accessControl": {"entries": ["other-principal"]}}]}}),
        serde_json::json!({"tunnel": {"ports": [{"portNumber": port}], "accessControl": {}}}),
    ] {
        fs::write(fixture.root.join("tunnel-metadata"), metadata.to_string()).unwrap();
        let output = fixture.foreground_server().output().unwrap();
        assert!(!output.status.success(), "{metadata}");
        assert!(!fixture.root.join("port-exists").exists(), "{metadata}");
    }
    fs::remove_file(fixture.root.join("tunnel-metadata")).unwrap();
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(!commands.contains("create "));
    assert!(!commands.contains("host "));

    let mut server = Running::start(fixture.foreground_server());
    server.ready();
    assert!(fixture.root.join("port-exists").exists());
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(!commands.lines().any(|line| line.starts_with("create ")));
    assert!(commands.contains("port create fixture.use2 --port-number"));
    let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    wait_for_exit(&pid);
}

#[test]
fn server_requires_a_complete_port_list_when_show_omits_ports() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    for ports in [None, Some(serde_json::Value::Null)] {
        let mut tunnel = serde_json::json!({"tunnelId": "fixture", "clusterId": "use2"});
        if let Some(ports) = ports {
            tunnel["ports"] = ports;
        }
        fs::write(
            fixture.root.join("tunnel-metadata"),
            serde_json::json!({"tunnel": tunnel}).to_string(),
        )
        .unwrap();
        for listed in [
            serde_json::json!({"ports": [{"portNumber": 1}]}),
            serde_json::json!({"ports": [{"portNumber": fixture.port}, {"portNumber": 1}]}),
        ] {
            fs::write(fixture.root.join("port-list"), listed.to_string()).unwrap();
            let output = fixture.foreground_server().output().unwrap();
            assert!(!output.status.success(), "{listed}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("dedicated tunnel"));
            assert!(!fixture.root.join("port-exists").exists());
            assert!(!fixture.root.join("host-pid").exists());
        }
    }
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(commands.contains("port list fixture.use2 --json\n"));
    assert!(!commands.contains("port create "));
    assert!(!commands.contains("host "));
}

#[test]
fn server_checks_listed_port_acl_even_when_show_omits_ports() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    fs::write(fixture.root.join("port-exists"), "").unwrap();
    for ports in [None, Some(serde_json::Value::Null)] {
        let mut tunnel = serde_json::json!({"tunnelId": "fixture", "clusterId": "use2"});
        if let Some(ports) = ports {
            tunnel["ports"] = ports;
        }
        fs::write(
            fixture.root.join("tunnel-metadata"),
            serde_json::json!({"tunnel": tunnel}).to_string(),
        )
        .unwrap();
        fs::write(
            fixture.root.join("acl-port"),
            r#"{"accessControlEntries":[{"type":"Anonymous"}]}"#,
        )
        .unwrap();
        let output = fixture.foreground_server().output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("owner-only"));
        assert!(!fixture.root.join("host-pid").exists());
    }
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(commands.contains(&format!(
        "access list fixture.use2 --port-number {} --json",
        fixture.port
    )));
    assert!(!commands.contains("port create "));
    assert!(!commands.contains("host "));
}

#[test]
fn server_creates_a_port_from_an_explicit_empty_port_list() {
    for response in [
        r#"{"warning":"No ports found for tunnel."}"#,
        r#"{"ports":[]}"#,
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
        fs::write(
            fixture.root.join("tunnel-metadata"),
            r#"{"tunnel":{"tunnelId":"fixture","clusterId":"use2","ports":null}}"#,
        )
        .unwrap();
        fs::write(fixture.root.join("port-list"), response).unwrap();
        fs::write(
            fixture.root.join("port-list-after-create"),
            serde_json::json!({"ports": [{"portNumber": fixture.port, "protocol": "auto"}]})
                .to_string(),
        )
        .unwrap();
        let mut server = Running::start(fixture.foreground_server());
        server.ready();
        let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
        assert_eq!(
            commands
                .lines()
                .filter(|line| line.starts_with("port list "))
                .count(),
            2
        );
        assert!(commands.contains(&format!(
            "access list fixture.use2 --port-number {} --json",
            fixture.port
        )));
        let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
        server.child.kill().unwrap();
        server.child.wait().unwrap();
        wait_for_exit(&pid);
    }
}

#[test]
fn server_fails_closed_on_port_list_errors_and_rechecks_after_creation() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    for response in [
        "not json",
        "{}",
        "null",
        r#"{"ports":null}"#,
        r#"{"warning":"other"}"#,
        r#"{"warning":"No ports found for tunnel.","error":"failed"}"#,
        r#"{"ports":[],"continuationToken":"more"}"#,
        r#"{"ports":[{}]}"#,
    ] {
        fs::write(fixture.root.join("port-list"), response).unwrap();
        let output = fixture.foreground_server().output().unwrap();
        assert!(!output.status.success(), "{response}");
        assert!(!fixture.root.join("port-exists").exists());
        assert!(!fixture.root.join("host-pid").exists());
    }
    fs::remove_file(fixture.root.join("port-list")).unwrap();
    fs::write(fixture.root.join("port-list-error"), "").unwrap();
    let output = fixture.foreground_server().output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Failed to retrieve tunnel port list")
    );
    assert!(!fixture.root.join("port-exists").exists());
    fs::remove_file(fixture.root.join("port-list-error")).unwrap();

    fs::write(
        fixture.root.join("port-list-after-create"),
        serde_json::json!({"ports": [{"portNumber": fixture.port}, {"portNumber": 1}]}).to_string(),
    )
    .unwrap();
    let output = fixture.foreground_server().output().unwrap();
    assert!(!output.status.success());
    assert!(fixture.root.join("port-exists").exists());
    assert!(!fixture.root.join("host-pid").exists());
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(!commands.contains("host "));
}

#[test]
fn server_rechecks_port_list_before_retrying_host() {
    for response in ["extra", "malformed", "error"] {
        let fixture = Fixture::new();
        let mut command = fixture.foreground_server();
        command.args(["--cooldown", "0", "--max-restarts", "1"]);
        let mut server = Running::start(command);
        server.ready();
        let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
        match response {
            "extra" => fs::write(
                fixture.root.join("port-list"),
                serde_json::json!({"ports": [{"portNumber": fixture.port}, {"portNumber": 1}]})
                    .to_string(),
            )
            .unwrap(),
            "malformed" => fs::write(fixture.root.join("port-list"), "{}").unwrap(),
            "error" => fs::write(fixture.root.join("port-list-error"), "").unwrap(),
            _ => unreachable!(),
        }
        trigger_host_failure(&fixture, "exit");
        let result = server.output();
        assert!(!result.status.success(), "{response}");
        wait_for_exit(&pid);
        assert_eq!(
            fs::read_to_string(fixture.root.join("host-attempts")).unwrap(),
            "1"
        );
    }
}

#[test]
fn server_requires_explicit_tunnel_and_port_access_lists() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    fs::write(fixture.root.join("port-exists"), "").unwrap();
    let metadata = serde_json::json!({"tunnel": {
        "tunnelId": "fixture", "clusterId": "use2",
        "accessControl": null, "ports": [{"portNumber": fixture.port}]
    }});
    fs::write(fixture.root.join("tunnel-metadata"), metadata.to_string()).unwrap();
    let anonymous = r#"{"accessControlEntries":[{"type":"Anonymous","isDeny":false,"isInherited":false,"subjects":[],"scopes":["connect"]}]}"#;
    for scope in ["tunnel", "port"] {
        for response in [
            anonymous,
            "{}",
            "null",
            r#"{"accessControlEntries":null}"#,
            r#"{"accessControlEntries":{}}"#,
            "not json",
        ] {
            fs::write(fixture.root.join(format!("acl-{scope}")), response).unwrap();
            let output = fixture.foreground_server().output().unwrap();
            assert!(!output.status.success(), "{scope}: {response}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            let expected = if response == anonymous {
                "owner-only"
            } else if response == "not json" {
                "Invalid access policy JSON"
            } else {
                "missing accessControlEntries array"
            };
            assert!(stderr.contains(expected), "{scope}: {response}: {stderr}");
            let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
            let query = if scope == "port" {
                format!(
                    "access list fixture.use2 --port-number {} --json",
                    fixture.port
                )
            } else {
                "access list fixture.use2 --json".into()
            };
            assert!(commands.lines().any(|line| line == query), "{commands}");
            assert!(
                !fixture.root.join("host-pid").exists(),
                "{scope}: {response}"
            );
        }
        fs::remove_file(fixture.root.join(format!("acl-{scope}"))).unwrap();
        fs::write(fixture.root.join(format!("acl-{scope}-error")), "").unwrap();
        let output = fixture.foreground_server().output().unwrap();
        assert!(!output.status.success(), "{scope}: unavailable");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Failed to retrieve tunnel access policy")
        );
        assert!(!fixture.root.join("host-pid").exists());
        fs::remove_file(fixture.root.join(format!("acl-{scope}-error"))).unwrap();
    }
    let mut server = Running::start(fixture.foreground_server());
    server.ready();
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(commands.contains("access list fixture.use2 --json\n"));
    assert!(commands.contains(&format!(
        "access list fixture.use2 --port-number {} --json\n",
        fixture.port
    )));
    assert!(!commands.contains("port create "));
    let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    wait_for_exit(&pid);
}

#[test]
fn server_rejects_incomplete_or_unrecognized_access_responses() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    fs::write(fixture.root.join("port-exists"), "").unwrap();
    for scope in ["tunnel", "port"] {
        for response in [
            r#"{"accessControlEntries":[],"error":"failed"}"#,
            r#"{"accessControlEntries":[],"error":null}"#,
            r#"{"accessControlEntries":[],"errors":[]}"#,
            r#"{"accessControlEntries":[],"warning":"partial"}"#,
            r#"{"accessControlEntries":[],"continuationToken":"more"}"#,
            r#"{"accessControlEntries":[],"continuationToken":null}"#,
            r#"{"accessControlEntries":[],"nextLink":"more"}"#,
            r#"{"accessControlEntries":[],"nextPage":2}"#,
            r#"{"accessControlEntries":[],"unknown":{"error":"failed"}}"#,
            r#"{"accessControlEntries":[],"tunnelId":"other.use2"}"#,
            r#"{"accessControlEntries":[],"portNumber":"invalid"}"#,
            r#"{"accessControlEntries":[],"accessControlEntries":[]}"#,
        ] {
            fs::write(fixture.root.join(format!("acl-{scope}")), response).unwrap();
            let output = fixture.foreground_server().output().unwrap();
            assert!(!output.status.success(), "{scope}: {response}");
            assert!(
                !fixture.root.join("host-pid").exists(),
                "{scope}: {response}"
            );
        }
        let wrong_port = if fixture.port == 1 { 2 } else { 1 };
        fs::write(
            fixture.root.join(format!("acl-{scope}")),
            serde_json::json!({"accessControlEntries":[], "portNumber":wrong_port}).to_string(),
        )
        .unwrap();
        let output = fixture.foreground_server().output().unwrap();
        assert!(!output.status.success(), "{scope}: wrong port");
        assert!(!fixture.root.join("host-pid").exists());
        fs::remove_file(fixture.root.join(format!("acl-{scope}"))).unwrap();
    }
    fs::remove_file(fixture.root.join("port-exists")).unwrap();
    for response in [
        r#"{"accessControlEntries":[],"error":"failed"}"#,
        r#"{"accessControlEntries":[],"continuationToken":"more"}"#,
        r#"{"accessControlEntries":[],"portNumber":1}"#,
    ] {
        fs::write(fixture.root.join("acl-tunnel"), response).unwrap();
        let output = fixture.foreground_server().output().unwrap();
        assert!(!output.status.success(), "{response}");
        assert!(!fixture.root.join("port-exists").exists(), "{response}");
        assert!(!fixture.root.join("host-pid").exists(), "{response}");
    }
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(!commands.contains("port create "));
    assert!(!commands.contains("host "));
}

#[test]
fn server_accepts_access_lists_with_matching_identity_metadata() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    fs::write(fixture.root.join("port-exists"), "").unwrap();
    fs::write(
        fixture.root.join("acl-tunnel"),
        r#"{"accessControlEntries":[],"tunnelId":"fixture.use2"}"#,
    )
    .unwrap();
    fs::write(
        fixture.root.join("acl-port"),
        serde_json::json!({"accessControlEntries":[], "tunnelId":"fixture.use2", "portNumber":fixture.port}).to_string(),
    )
    .unwrap();
    let mut server = Running::start(fixture.foreground_server());
    server.ready();
    let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    wait_for_exit(&pid);
}

#[test]
fn server_rechecks_access_after_port_creation() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
    let anonymous =
        r#"{"accessControlEntries":[{"type":"Anonymous","isDeny":false,"scopes":["connect"]}]}"#;
    fs::write(fixture.root.join("acl-tunnel"), anonymous).unwrap();
    let rejected = fixture.foreground_server().output().unwrap();
    assert!(!rejected.status.success());
    assert!(!fixture.root.join("port-exists").exists());
    fs::remove_file(fixture.root.join("acl-tunnel")).unwrap();
    for scope in ["tunnel", "port"] {
        for response in [
            anonymous,
            r#"{"accessControlEntries":[],"error":"failed"}"#,
            r#"{"accessControlEntries":[],"continuationToken":"more"}"#,
        ] {
            fs::remove_file(fixture.root.join("port-exists")).ok();
            fs::write(
                fixture.root.join(format!("acl-{scope}-after-create")),
                response,
            )
            .unwrap();
            let rejected = fixture.foreground_server().output().unwrap();
            assert!(!rejected.status.success(), "{scope}: {response}");
            assert!(fixture.root.join("port-exists").exists());
            assert!(!fixture.root.join("host-pid").exists());
            fs::remove_file(fixture.root.join(format!("acl-{scope}-after-create"))).unwrap();
            fs::remove_file(fixture.root.join(format!("acl-{scope}"))).unwrap();
        }
    }
    let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
    assert!(commands.contains("port create fixture.use2 --port-number"));
    assert!(commands.contains(&format!(
        "access list fixture.use2 --port-number {} --json",
        fixture.port
    )));
    assert!(!commands.contains("host "));
}

#[test]
fn server_rechecks_access_before_retrying_host() {
    for (scope, response, expected) in [
        (
            "tunnel",
            r#"{"accessControlEntries":[{"type":"Anonymous"}]}"#,
            "owner-only",
        ),
        (
            "port",
            r#"{"accessControlEntries":[{"type":"Anonymous"}]}"#,
            "owner-only",
        ),
        (
            "tunnel",
            r#"{"accessControlEntries":[],"error":"failed"}"#,
            "Unrecognized access policy",
        ),
        (
            "port",
            r#"{"accessControlEntries":[],"continuationToken":"more"}"#,
            "Unrecognized access policy",
        ),
    ] {
        let fixture = Fixture::new();
        let mut command = fixture.foreground_server();
        command.args(["--cooldown", "0", "--max-restarts", "1"]);
        let mut server = Running::start(command);
        server.ready();
        let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
        fs::write(fixture.root.join(format!("acl-{scope}")), response).unwrap();
        trigger_host_failure(&fixture, "exit");
        let result = server.output();
        assert!(!result.status.success(), "{scope}: {response}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{scope}: {response}"
        );
        wait_for_exit(&pid);
        assert_eq!(
            fs::read_to_string(fixture.root.join("host-attempts")).unwrap(),
            "1"
        );
        let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
        assert_eq!(
            commands
                .lines()
                .filter(|line| line.starts_with("host "))
                .count(),
            1
        );
        assert!(
            commands
                .lines()
                .filter(|line| line.starts_with("access list fixture.use2 --json"))
                .count()
                >= 3
        );
    }
}

#[test]
fn server_creates_in_the_requested_relay_and_preserves_bare_name_behavior() {
    let launch = |fixture: &Fixture, tunnel: &str| {
        let mut command = fixture.command("server");
        command.args([
            "--foreground",
            "--tunnel",
            tunnel,
            "--port",
            &fixture.port.to_string(),
            "--timeout",
            "5",
        ]);
        command
    };
    for (tunnel, cluster, existing) in [
        ("fixture.inc1", "inc1", false),
        ("fixture.asse", "asse", false),
        ("fixture", "use2", false),
        ("fixture", "use2", true),
    ] {
        let fixture = Fixture::new();
        if existing {
            fs::write(fixture.root.join("tunnel-exists"), "").unwrap();
        }
        let mut server = Running::start(launch(&fixture, tunnel));
        server.ready();
        let commands = fs::read_to_string(fixture.root.join("tunnel-commands")).unwrap();
        if existing {
            assert!(!commands.lines().any(|line| line.starts_with("create ")));
        } else if tunnel.contains('.') {
            assert!(commands.contains(&format!(
                "create fixture --service-uri https://{cluster}.rel.tunnels.api.visualstudio.com --json\n"
            )), "{commands}");
        } else {
            assert!(commands.contains("create fixture --json\n"), "{commands}");
            assert!(!commands.contains("--service-uri"), "{commands}");
        }
        assert!(
            commands.contains(&format!("host fixture.{cluster}\n")),
            "{commands}"
        );
        assert!(
            server
                .log
                .lock()
                .unwrap()
                .contains(&format!("--tunnel fixture.{cluster}"))
        );
        let pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
        server.child.kill().unwrap();
        server.child.wait().unwrap();
        wait_for_exit(&pid);
    }

    let fixture = Fixture::new();
    let rejected = launch(&fixture, "fixture.inc1")
        .env("HERDR_TEST_FIXTURE_CREATED_CLUSTER", "use2")
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("Requested relay inc1"));
    assert!(!fixture.root.join("port-exists").exists());
    assert!(!fixture.root.join("host-pid").exists());
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for fixture state"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn disconnected_clients_release_all_bridge_slots_when_hc_ignores_eof() {
    let fixture = Fixture::new();
    let guard = HcGuard(fixture.root.join("hc-guard"));
    fs::write(&guard.0, b"").unwrap();
    let mut command = fixture.connection("server");
    command
        .env("HERDR_TEST_FIXTURE_HANG_AFTER_EOF", "1")
        .env("HERDR_TEST_FIXTURE_HC_GUARD", "1");
    success(&command.output().unwrap());

    let mut sockets = Vec::new();
    for _ in 0..32 {
        let socket = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut socket = BufReader::new(socket);
        socket
            .get_mut()
            .write_all(b"{\"protocol\":\"herdr-devtunnel/1\",\"session\":\"work\"}\nprobe")
            .unwrap();
        let mut hello = String::new();
        socket.read_line(&mut hello).unwrap();
        let hello: serde_json::Value = serde_json::from_str(&hello).unwrap();
        assert_eq!(hello["ok"], true);
        let mut probe = [0; 5];
        socket.read_exact(&mut probe).unwrap();
        assert_eq!(&probe, b"probe");
        sockets.push(socket);
    }
    let pids = fs::read_to_string(fixture.root.join("hc-connections")).unwrap();
    assert_eq!(pids.lines().count(), 32);
    for socket in &mut sockets {
        socket.get_mut().shutdown(Shutdown::Write).unwrap();
    }
    for mut socket in sockets {
        let mut trailing = Vec::new();
        socket.read_to_end(&mut trailing).unwrap();
        assert!(trailing.is_empty());
    }
    for pid in pids.lines() {
        wait_for_exit(pid);
    }

    // A new client must still get a slot after the entire pool was occupied.
    let socket = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut socket = BufReader::new(socket);
    socket
        .get_mut()
        .write_all(b"{\"protocol\":\"herdr-devtunnel/1\",\"session\":\"work\"}\nreconnected")
        .unwrap();
    let mut hello = String::new();
    socket.read_line(&mut hello).unwrap();
    let hello: serde_json::Value = serde_json::from_str(&hello).unwrap();
    assert_eq!(hello["ok"], true);
    let mut echoed = [0; 11];
    socket.read_exact(&mut echoed).unwrap();
    assert_eq!(&echoed, b"reconnected");
    let pids = fs::read_to_string(fixture.root.join("hc-connections")).unwrap();
    let replacement_pid = pids.lines().last().unwrap();
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    success(&fixture.command("stop").output().unwrap());
    wait_for_exit(replacement_pid);
    wait_for_exit(&host_pid);
}

#[test]
fn managed_stop_terminates_live_hc_and_incomplete_handshakes() {
    let fixture = Fixture::new();
    let guard = HcGuard(fixture.root.join("hc-guard"));
    fs::write(&guard.0, b"").unwrap();
    let mut command = fixture.connection("server");
    command
        .env("HERDR_TEST_FIXTURE_HANG_AFTER_EOF", "1")
        .env("HERDR_TEST_FIXTURE_HC_GUARD", "1");
    success(&command.output().unwrap());

    let worker_pid = fixture.state()["pid"].as_u64().unwrap().to_string();
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    assert_eq!(fixture.state()["ready"], true);
    assert!(process_alive(&worker_pid));
    assert!(process_alive(&host_pid));
    let socket = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut socket = BufReader::new(socket);
    socket
        .get_mut()
        .write_all(b"{\"protocol\":\"herdr-devtunnel/1\",\"session\":\"work\"}\nprobe")
        .unwrap();
    let mut hello = String::new();
    socket.read_line(&mut hello).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&hello).unwrap()["ok"],
        true
    );
    let mut probe = [0; 5];
    socket.read_exact(&mut probe).unwrap();
    assert_eq!(&probe, b"probe");
    let connections = fs::read_to_string(fixture.root.join("hc-connections")).unwrap();
    let mut pids = connections.lines();
    let hc_pid = pids.next().unwrap();
    assert!(pids.next().is_none());
    assert!(process_alive(hc_pid));

    let mut pending = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
    pending.write_all(b"{\"protocol\":").unwrap();
    pending
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut byte = [0; 1];
    let error = pending.peek(&mut byte).unwrap_err();
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{error}"
    );
    pending
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("hc-connections")).unwrap(),
        connections
    );
    assert!(process_alive(hc_pid));

    let stopped = fixture
        .command("stop")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    for connection in [&mut pending as &mut dyn Read, &mut socket as &mut dyn Read] {
        match connection.read(&mut byte) {
            Ok(count) => assert_eq!(count, 0),
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ),
                "{error}"
            ),
        }
    }
    let stopped = stopped.wait_with_output().unwrap();
    success(&stopped);
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("Server 'default' stopped."));
    assert!(
        !process_alive(hc_pid),
        "hc child {hc_pid} survived successful stop"
    );
    assert_eq!(fixture.state()["ready"], false);
    assert_eq!(fixture.state()["message"], "stopped");
    assert_eq!(
        fs::read_to_string(fixture.root.join("hc-connections")).unwrap(),
        connections
    );
    wait_for_exit(&worker_pid);
    wait_for_exit(&host_pid);
}

fn trigger_host_failure(fixture: &Fixture, failure: &str) {
    fs::write(fixture.root.join("host-fail.tmp"), failure).unwrap();
    fs::rename(
        fixture.root.join("host-fail.tmp"),
        fixture.root.join("host-fail"),
    )
    .unwrap();
}

fn reconnect_machines(fixture: &Fixture, names: &[&str]) {
    #[cfg(not(windows))]
    let target = {
        let exe = fixture.cli.to_str().unwrap().replace('\'', "'\\''");
        format!("'{exe}' 'herdr' 'connect' '--tunnel' 'fixture.use2'")
    };
    #[cfg(windows)]
    let target = format!(
        "\"{}\" \"herdr\" \"connect\" \"--tunnel\" \"fixture.use2\"",
        fixture.cli.display()
    );
    let mut machines: Vec<_> = names
        .iter()
        .map(|name| serde_json::json!({"name": name, "target": target, "via": true}))
        .collect();
    machines.extend([
        serde_json::json!({"name": "ssh", "target": target, "via": false}),
        serde_json::json!({"name": "other", "target": "'provider' 'connect'", "via": true}),
        serde_json::json!({"name": "mentions-herdr", "target": format!("'echo' {target}"), "via": true}),
    ]);
    fs::write(
        fixture.root.join("api-machines"),
        serde_json::to_vec(&serde_json::json!({"machines": machines})).unwrap(),
    )
    .unwrap();
}

fn reconnected_names(fixture: &Fixture) -> Vec<String> {
    fs::read_to_string(fixture.root.join("api-calls"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|request| request["method"] == "machine.connect")
        .map(|request| request["params"]["machine"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn reconnect_all_restarts_running_servers_and_preserves_client_registrations() {
    let fixture = Fixture::new();
    let started = fixture
        .connection("server")
        .args([
            "--cooldown",
            "0",
            "--max-restarts",
            "4",
            "--pattern",
            "custom failure",
        ])
        .output()
        .unwrap();
    success(&started);
    let directory = fixture.root.join(".herdr-devtunnel/server-default");
    let config = fs::read(directory.join("config.json")).unwrap();
    let old_worker = fixture.state()["pid"].as_u64().unwrap().to_string();
    let old_host = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    let second_port = allocate_port();
    let second_dt = fixture
        .root
        .join(format!("devtunnel-second{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&fixture.dt, &second_dt).unwrap();
    fs::write(fixture.root.join("second-port"), second_port.to_string()).unwrap();
    success(
        &fixture
            .command_with_devtunnel("server", &second_dt)
            .args([
                "--name",
                "second",
                "--tunnel",
                "fixture.use2",
                "--port",
                &second_port.to_string(),
            ])
            .output()
            .unwrap(),
    );
    let second_directory = fixture.root.join(".herdr-devtunnel/server-second");
    let second_state = || -> serde_json::Value {
        serde_json::from_slice(&fs::read(second_directory.join("state.json")).unwrap()).unwrap()
    };
    let second_worker = second_state()["pid"].as_u64().unwrap().to_string();
    let second_config = fs::read(second_directory.join("config.json")).unwrap();
    let stopped = fixture.root.join(".herdr-devtunnel/server-stopped");
    fs::create_dir(&stopped).unwrap();
    // No configuration is needed for deliberately stopped profiles.
    reconnect_machines(&fixture, &["workbox", "second"]);
    let machines_before = fs::read(fixture.root.join("api-machines")).unwrap();
    let output = fixture
        .command("reconnect")
        .args(["--all", "--session", "local"])
        .output()
        .unwrap();
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Reconnecting server 'default'"));
    assert!(stdout.contains("Reconnecting server 'second'"));
    assert!(stdout.contains("Machine 'workbox' is connected."));
    assert_ne!(
        fixture.state()["pid"].as_u64().unwrap().to_string(),
        old_worker
    );
    assert_eq!(fixture.state()["ready"], true);
    assert_ne!(
        second_state()["pid"].as_u64().unwrap().to_string(),
        second_worker
    );
    assert_eq!(second_state()["ready"], true);
    wait_for_exit(&old_worker);
    wait_for_exit(&old_host);
    wait_for_exit(&second_worker);
    assert_eq!(fs::read(directory.join("config.json")).unwrap(), config);
    assert_eq!(
        fs::read(second_directory.join("config.json")).unwrap(),
        second_config
    );
    assert!(!stopped.join("state.json").exists());
    assert_eq!(reconnected_names(&fixture), ["workbox", "second"]);
    assert_eq!(
        fs::read(fixture.root.join("api-machines")).unwrap(),
        machines_before
    );
    let calls = fs::read_to_string(fixture.root.join("api-calls")).unwrap();
    assert!(calls.contains("connect --session local"));
    assert!(!calls.contains("\"machine.rm\""));
    assert!(!calls.contains("\"machine.add\""));
    assert!(!fixture.root.join("machine-args").exists());
}

#[test]
fn reconnect_session_alias_preserves_default_and_socket_selection() {
    let fixture = Fixture::new();
    reconnect_machines(&fixture, &["workbox"]);
    for (subcommand, option, expected) in [
        ("reconnect", None, "default"),
        ("reconnect", Some("--session"), "local"),
        ("reconnect", Some("--local-session"), "local"),
        ("r", Some("--local-session"), "local"),
    ] {
        let mut command = fixture.command(subcommand);
        command.args(["--all", "--role", "client"]);
        if let Some(option) = option {
            command.args([option, "local"]);
        }
        let output = command
            .env("HC_SESSION", "inherited")
            .env("HC_SOCKET", fixture.root.join("wrong.sock"))
            .output()
            .unwrap();
        success(&output);
        let calls = fs::read_to_string(fixture.root.join("api-calls")).unwrap();
        assert_eq!(
            calls
                .matches(&format!("connect --session {expected}\n"))
                .count(),
            2
        );
        assert_eq!(reconnected_names(&fixture), ["workbox"]);
        assert_eq!(
            fs::read_to_string(fixture.root.join("api-HC_SESSION")).unwrap(),
            "Some:inherited"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("api-HC_SOCKET")).unwrap(),
            "None"
        );
        fs::remove_file(fixture.root.join("api-calls")).unwrap();
    }
}

#[test]
fn reconnect_clients_continues_after_connection_protocol_and_timeout_failures() {
    let fixture = Fixture::new();
    reconnect_machines(
        &fixture,
        &["offline", "rpc-error", "malformed", "slow", "healthy"],
    );
    let output = fixture
        .command("reconnect")
        .args(["--all", "--role", "client", "--timeout", "3"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fixture tunnel offline"), "{stderr}");
    assert!(stderr.contains("unsupported method"), "{stderr}");
    assert!(stderr.contains("Invalid hc API response"), "{stderr}");
    assert!(stderr.contains("timed out after 3 seconds"), "{stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Machine 'healthy' is connected."));
    assert_eq!(
        reconnected_names(&fixture),
        ["offline", "rpc-error", "malformed", "slow", "healthy"]
    );
    assert!(!fixture.root.join(".herdr-devtunnel").exists());
}

#[test]
fn reconnect_all_continues_after_a_broken_server_and_supports_server_only() {
    let fixture = Fixture::new();
    success(&fixture.connection("server").output().unwrap());
    let directory = fixture.root.join(".herdr-devtunnel/server-default");
    let config = fs::read(directory.join("config.json")).unwrap();
    fs::write(directory.join("config.json"), b"invalid").unwrap();
    reconnect_machines(&fixture, &["workbox"]);
    let output = fixture.command("reconnect").arg("--all").output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Invalid managed Herdr configuration")
    );
    assert!(fixture.state()["ready"].as_bool().unwrap());
    assert_eq!(reconnected_names(&fixture), ["workbox"]);
    fs::write(directory.join("config.json"), config).unwrap();
    fs::remove_file(fixture.root.join("api-calls")).unwrap();
    let output = fixture
        .command("reconnect")
        .args(["--all", "--role", "server"])
        .output()
        .unwrap();
    success(&output);
    assert!(!fixture.root.join("api-calls").exists());
    assert_eq!(fixture.state()["ready"], true);
}

#[test]
fn reconnect_requires_all_and_reports_empty_scopes() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .command("reconnect")
            .output()
            .unwrap()
            .status
            .success()
    );
    for subcommand in ["reconnect", "r"] {
        let output = fixture.command(subcommand).arg("--all").output().unwrap();
        success(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("No running managed Herdr servers found."));
        assert!(stdout.contains("No registered Herdr dev-tunnel machines found."));
    }
}

#[test]
fn managed_server_recovers_only_the_tunnel_and_obeys_restart_budget() {
    let fixture = Fixture::new();
    let launch = || {
        let mut command = fixture.connection("server");
        command.args([
            "--cooldown",
            "0",
            "--max-restarts",
            "3",
            "--pattern",
            "fixture tunnel is unhealthy",
        ]);
        command
    };
    let started = launch().output().unwrap();
    success(&started);
    assert!(String::from_utf8_lossy(&started.stdout).contains("started in background"));
    let worker_pid = fixture.state()["pid"].as_u64().unwrap().to_string();
    success(&launch().output().unwrap());
    assert_eq!(
        fs::read_to_string(fixture.root.join("host-attempts")).unwrap(),
        "1"
    );
    let conflicting = fixture.connection("server").output().unwrap();
    assert!(!conflicting.status.success());
    assert!(String::from_utf8_lossy(&conflicting.stderr).contains("different settings"));
    success(
        &fixture
            .command("stop")
            .args(["--name", "unrelated"])
            .output()
            .unwrap(),
    );
    assert!(process_alive(&worker_pid));

    // Recently exited commands can briefly retain the executable mapping.
    // The managed worker itself must not keep the installed binary locked.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::copy(env!("CARGO_BIN_EXE_hd"), &fixture.cli) {
            Ok(_) => break,
            Err(error)
                if (error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    || error.raw_os_error() == Some(32))
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("installed CLI remains locked: {error}"),
        }
    }
    let mut socket = TcpStream::connect(("127.0.0.1", fixture.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket
        .write_all(b"{\"protocol\":\"herdr-devtunnel/1\",\"session\":\"work\"}\n")
        .unwrap();
    let mut socket = BufReader::new(socket);
    let mut hello = String::new();
    socket.read_line(&mut hello).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&hello).unwrap()["ok"],
        true
    );
    socket.get_mut().write_all(b"probe").unwrap();
    let mut probe = [0; 5];
    socket.read_exact(&mut probe).unwrap();
    assert_eq!(&probe, b"probe");
    let connections = fs::read_to_string(fixture.root.join("hc-connections")).unwrap();
    for (index, failure) in ["success-exit", "auth", "custom"].iter().enumerate() {
        let previous_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
        trigger_host_failure(&fixture, failure);
        wait_until(|| {
            let state = fixture.state();
            state["ready"] == true && state["restarts"] == index + 1
        });
        wait_for_exit(&previous_pid);
        let bytes = [0, 255, index as u8, b'\n'];
        socket.get_mut().write_all(&bytes).unwrap();
        let mut echo = [0; 4];
        socket.read_exact(&mut echo).unwrap();
        assert_eq!(echo, bytes);
        assert_eq!(
            fs::read_to_string(fixture.root.join("hc-connections")).unwrap(),
            connections
        );
    }
    let status = fixture.command("status").output().unwrap();
    success(&status);
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(status.contains("ready=true; restarts=3"));
    assert_eq!(
        fixture.state()["pid"].as_u64().unwrap().to_string(),
        worker_pid
    );

    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    trigger_host_failure(&fixture, "exit");
    wait_for_exit(&worker_pid);
    wait_for_exit(&host_pid);
    assert_eq!(fixture.state()["ready"], false);
    assert!(
        fixture.state()["message"]
            .as_str()
            .unwrap()
            .contains("Reached max restarts (3)")
    );
    let status = fixture.command("status").output().unwrap();
    success(&status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("is not running"));
}

#[test]
fn managed_server_handles_readiness_timeout_and_stop_during_cooldown() {
    let fixture = Fixture::new();
    let launch = |mode: &str, cooldown: &str, max_restarts: &str| {
        let mut command = fixture.command("server");
        command
            .args([
                "--tunnel",
                "fixture.use2",
                "--port",
                &fixture.port.to_string(),
                "--session",
                "work",
                "--timeout",
                "1",
                "--cooldown",
                cooldown,
                "--max-restarts",
                max_restarts,
            ])
            .env("HERDR_TEST_FIXTURE_HOST_MODE", mode);
        command
    };
    success(&launch("no-ready-once", "2", "3").output().unwrap());
    assert_eq!(fixture.state()["ready"], true);
    assert_eq!(fixture.state()["restarts"], 1);
    assert_eq!(
        fs::read_to_string(fixture.root.join("host-attempts")).unwrap(),
        "2"
    );
    let worker_pid = fixture.state()["pid"].as_u64().unwrap().to_string();
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    trigger_host_failure(&fixture, "auth");
    wait_until(|| fixture.state()["message"] == "waiting to restart tunnel host");
    let status = fixture.command("status").output().unwrap();
    success(&status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("ready=false"));
    success(&fixture.command("stop").output().unwrap());
    wait_for_exit(&worker_pid);
    wait_for_exit(&host_pid);
    assert_eq!(fixture.state()["message"], "stopped");
    assert_eq!(
        fs::read_to_string(fixture.root.join("host-attempts")).unwrap(),
        "2"
    );

    let failed = launch("never-ready", "0", "1").output().unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("exited during startup"));
    assert_eq!(fixture.state()["ready"], false);
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    wait_for_exit(&host_pid);
    assert!(
        fixture.state()["message"]
            .as_str()
            .unwrap()
            .contains("readiness timed out")
    );

    let starting = Running::start(launch("never-ready", "30", "0"));
    wait_until(|| {
        fs::read_to_string(fixture.root.join("host-attempts")).is_ok_and(|attempts| attempts == "5")
    });
    wait_until(|| {
        let output = fixture.command("status").output().unwrap();
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("ready=false")
    });
    let worker_pid = fixture.state()["pid"].as_u64().unwrap().to_string();
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    success(&fixture.command("stop").output().unwrap());
    assert!(!starting.output().status.success());
    wait_for_exit(&worker_pid);
    wait_for_exit(&host_pid);
    assert_eq!(fixture.state()["message"], "stopped");
}

#[test]
fn matching_server_fails_during_startup_and_recovery_then_succeeds_when_ready() {
    let fixture = Fixture::new();
    let launch = |mode: &str, cooldown: &str| {
        let mut command = fixture.connection("server");
        command
            .args(["--cooldown", cooldown])
            .env("HERDR_TEST_FIXTURE_HOST_MODE", mode);
        command
    };

    let starting = Running::start(launch("never-ready", "30"));
    wait_until(|| {
        let output = fixture.command("status").output().unwrap();
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("ready=false")
    });
    let worker_pid = fixture.state()["pid"].as_u64().unwrap();
    let retry = launch("never-ready", "30").output().unwrap();
    assert!(!retry.status.success());
    let stderr = String::from_utf8_lossy(&retry.stderr);
    assert!(stderr.contains("not ready"), "{stderr}");
    assert!(stderr.contains("status --name default"), "{stderr}");
    assert!(stderr.contains("retry `server`"), "{stderr}");
    assert!(!String::from_utf8_lossy(&retry.stdout).contains("already running"));
    assert_eq!(fixture.state()["pid"].as_u64(), Some(worker_pid));
    assert_eq!(fixture.state()["ready"], false);
    success(&fixture.command("stop").output().unwrap());
    assert!(!starting.output().status.success());

    success(&launch("", "120").output().unwrap());
    let worker_pid = fixture.state()["pid"].as_u64().unwrap();
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    trigger_host_failure(&fixture, "exit");
    wait_until(|| fixture.state()["message"] == "waiting to restart tunnel host");
    let retry = launch("", "120").output().unwrap();
    assert!(!retry.status.success());
    assert!(String::from_utf8_lossy(&retry.stderr).contains("not ready"));
    assert_eq!(fixture.state()["pid"].as_u64(), Some(worker_pid));
    assert_eq!(fixture.state()["ready"], false);
    success(&fixture.command("stop").output().unwrap());
    wait_for_exit(&worker_pid.to_string());
    wait_for_exit(&host_pid);

    success(&launch("", "0").output().unwrap());
    let worker_pid = fixture.state()["pid"].as_u64().unwrap();
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    trigger_host_failure(&fixture, "exit");
    wait_until(|| {
        let state = fixture.state();
        state["ready"] == true && state["restarts"] == 1
    });
    wait_for_exit(&host_pid);
    let ready = launch("", "0").output().unwrap();
    success(&ready);
    assert!(String::from_utf8_lossy(&ready.stdout).contains("ready=true"));
    assert_eq!(fixture.state()["pid"].as_u64(), Some(worker_pid));
    let host_pid = fs::read_to_string(fixture.root.join("host-pid")).unwrap();
    success(&fixture.command("stop").output().unwrap());
    wait_for_exit(&worker_pid.to_string());
    wait_for_exit(&host_pid);
}
