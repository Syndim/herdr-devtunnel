use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
fn windows_command_line(line: &str) -> Vec<std::ffi::OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    let wide: Vec<u16> = std::ffi::OsStr::new(line)
        .encode_wide()
        .chain([0])
        .collect();
    let mut count = 0;
    unsafe {
        let argv = CommandLineToArgvW(wide.as_ptr(), &mut count);
        assert!(!argv.is_null());
        let words = std::slice::from_raw_parts(argv, count as usize)
            .iter()
            .map(|&word| {
                let mut len = 0;
                while *word.add(len) != 0 {
                    len += 1;
                }
                std::ffi::OsString::from_wide(std::slice::from_raw_parts(word, len))
            })
            .collect();
        assert!(LocalFree(argv.cast()).is_null());
        words
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(std::env::var_os("HERDR_TEST_FIXTURE_ROOT").unwrap());
    let name = std::env::current_exe().unwrap();
    let port = if name.file_stem().unwrap() == "devtunnel-second" {
        std::fs::read_to_string(root.join("second-port")).unwrap()
    } else {
        std::env::var("HERDR_TEST_FIXTURE_PORT").unwrap()
    };
    let cluster = match std::fs::read_to_string(root.join("tunnel-cluster")) {
        Ok(cluster) => cluster,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "use2".into(),
        Err(error) => panic!("{error}"),
    };
    let is_hc = name
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .starts_with("hc");
    if !is_hc {
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("tunnel-commands"))
                .unwrap(),
            "{}",
            args.join(" ")
        )
        .unwrap();
    }
    match args.first().map(String::as_str) {
        Some("--version") => println!("fixture 0.1.17"),
        Some("user") => println!("Logged in as fixture"),
        Some("show") => {
            if let Ok(error) = std::env::var("HERDR_TEST_FIXTURE_LOOKUP_ERROR") {
                match error.as_str() {
                    "auth" => eprintln!("Authentication failed"),
                    "network" => eprintln!("Connection timed out"),
                    "unknown" => eprintln!("Unexpected tunnel lookup failure"),
                    "wrong-code" => {
                        let tunnel = &args[1];
                        if let Some((name, cluster)) = tunnel.split_once('.') {
                            eprintln!("Tunnel not found in {cluster}: {name}");
                        } else {
                            eprintln!("Tunnel not found: {tunnel}");
                        }
                    }
                    "malformed" => {
                        println!("{{}}");
                        return;
                    }
                    _ => panic!("unknown lookup error"),
                }
                std::process::exit(if error == "wrong-code" { 1 } else { 2 });
            }
            if !root.join("tunnel-exists").exists() {
                let tunnel = &args[1];
                if let Some((name, cluster)) = tunnel.split_once('.') {
                    eprintln!("Tunnel not found in {cluster}: {name}");
                } else {
                    eprintln!("Tunnel not found: {tunnel}");
                }
                std::process::exit(2);
            }
            if let Ok(metadata) = std::fs::read_to_string(root.join("tunnel-metadata")) {
                println!("{metadata}");
                return;
            }
            let ports = if root.join("port-exists").exists() {
                format!("[{{\"portNumber\":{port}}}]")
            } else if std::env::var_os("HERDR_TEST_FIXTURE_WRONG_PORT").is_some() {
                let wrong = if port == "1" { "2" } else { "1" };
                format!("[{{\"portNumber\":{wrong}}}]")
            } else {
                "[]".into()
            };
            let acl = if std::env::var_os("HERDR_TEST_FIXTURE_SHARED").is_some() {
                "[\"other-principal\"]"
            } else {
                "[]"
            };
            println!(
                "{{\"tunnel\":{{\"tunnelId\":\"fixture\",\"clusterId\":\"{cluster}\",\"ports\":{ports},\"accessControl\":{acl}}}}}"
            );
        }
        Some("create") => {
            let cluster = match args.iter().position(|arg| arg == "--service-uri") {
                Some(index) => args[index + 1]
                    .strip_prefix("https://")
                    .unwrap()
                    .strip_suffix(".rel.tunnels.api.visualstudio.com")
                    .unwrap(),
                None => "use2",
            };
            let cluster = std::env::var("HERDR_TEST_FIXTURE_CREATED_CLUSTER")
                .unwrap_or_else(|_| cluster.into());
            std::fs::write(root.join("tunnel-cluster"), &cluster).unwrap();
            std::fs::write(root.join("tunnel-exists"), "").unwrap();
            println!("{{\"tunnelId\":\"fixture\",\"clusterId\":\"{cluster}\"}}");
        }
        Some("access") if args.get(1).map(String::as_str) == Some("list") => {
            let scope = if args.iter().any(|arg| arg == "--port-number") {
                "port"
            } else {
                "tunnel"
            };
            if root.join(format!("acl-{scope}-error")).exists() {
                eprintln!("fixture access lookup failed");
                std::process::exit(1);
            }
            let response = std::fs::read_to_string(root.join(format!("acl-{scope}")))
                .unwrap_or_else(|_| "{\n  \"accessControlEntries\": []\n}".into());
            println!("{response}");
        }
        Some("port") if args.get(1).map(String::as_str) == Some("list") => {
            if root.join("port-list-error").exists() {
                eprintln!("fixture port list failed");
                std::process::exit(1);
            }
            if let Ok(response) = std::fs::read_to_string(root.join("port-list")) {
                println!("{response}");
            } else if root.join("port-exists").exists() {
                println!("{{\"ports\":[{{\"portNumber\":{port},\"protocol\":\"auto\"}}]}}");
            } else if std::env::var_os("HERDR_TEST_FIXTURE_WRONG_PORT").is_some() {
                let wrong = if port == "1" { "2" } else { "1" };
                println!("{{\"ports\":[{{\"portNumber\":{wrong},\"protocol\":\"auto\"}}]}}");
            } else {
                println!("{{\"warning\":\"No ports found for tunnel.\"}}");
            }
        }
        Some("port") if args.get(1).map(String::as_str) == Some("create") => {
            std::fs::write(root.join("port-exists"), "").unwrap();
            if let Ok(ports) = std::fs::read(root.join("port-list-after-create")) {
                std::fs::write(root.join("port-list"), ports).unwrap();
            }
            for scope in ["tunnel", "port"] {
                if let Ok(policy) = std::fs::read(root.join(format!("acl-{scope}-after-create"))) {
                    std::fs::write(root.join(format!("acl-{scope}")), policy).unwrap();
                }
            }
            println!("{{\"tunnelId\":\"fixture\",\"clusterId\":\"use2\"}}");
        }
        Some("machine") => {
            if args.iter().any(|a| a == "--help") {
                for key in ["HC_SESSION", "HC_SOCKET"] {
                    let value =
                        std::env::var(key).map_or_else(|_| "None".into(), |v| format!("Some:{v}"));
                    std::fs::write(root.join(format!("machine-help-{key}")), value).unwrap();
                }
                if std::env::var_os("HERDR_TEST_FIXTURE_LEGACY_HC").is_some() {
                    println!("--via");
                } else {
                    println!("--via Windows CommandLineToArgvW split, no shell");
                }
                return;
            }
            std::fs::write(root.join("machine-args"), args.join("\n")).unwrap();
            for key in ["HC_SESSION", "HC_SOCKET"] {
                let value =
                    std::env::var(key).map_or_else(|_| "None".into(), |v| format!("Some:{v}"));
                std::fs::write(root.join(format!("machine-{key}")), value).unwrap();
            }
            #[cfg(windows)]
            std::fs::write(root.join("machine-path"), std::env::var("PATH").unwrap()).unwrap();
            let via = &args[args.iter().position(|a| a == "--via").unwrap() + 1];
            let session = &args[args.iter().position(|a| a == "--session").unwrap() + 1];
            let bootstrap = format!(
                "sh -c 'p=$(command -v herdr-core) || p=$HOME/.local/bin/herdr-core; [ -x \"$p\" ] || {{ echo \"herdr-core is not installed here\" >&2; exit 127; }}; HC_SESSION={session} exec \"$p\" connect'"
            );
            #[cfg(not(windows))]
            let mut probe_command = {
                let mut command = Command::new("sh");
                command.args([
                    "-c",
                    &format!("{via} '{}'", bootstrap.replace('\'', "'\\''")),
                ]);
                command
            };
            #[cfg(windows)]
            let mut probe_command = {
                let words = windows_command_line(via);
                let mut command = Command::new(&words[0]);
                command.args(&words[1..]).arg(&bootstrap);
                command
            };
            let mut probe = probe_command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            probe
                .stdin
                .take()
                .unwrap()
                .write_all(b"fixture probe\0\xff")
                .unwrap();
            let result = probe.wait_with_output().unwrap();
            assert!(result.status.success());
            assert_eq!(result.stdout, b"fixture probe\0\xff");
        }
        Some("connect") if is_hc => {
            if std::env::var_os("HERDR_TEST_FIXTURE_REQUIRE_NO_HC_SOCKET").is_some() {
                assert!(std::env::var_os("HC_SOCKET").is_none());
                assert_eq!(args, ["connect", "--session", "work"]);
            }
            if args.iter().any(|a| a == "--help") {
                println!("--session");
                return;
            }
            if std::env::var_os("HERDR_TEST_FIXTURE_API").is_some() {
                api(&root, &args);
                return;
            }
            writeln!(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("hc-connections"))
                    .unwrap(),
                "{}",
                std::process::id()
            )
            .unwrap();
            if std::env::var_os("HERDR_TEST_FIXTURE_EARLY_CLOSE").is_some() {
                std::io::stdout().write_all(b"remote-closed").unwrap();
                return;
            }

            fn api(root: &std::path::Path, args: &[String]) {
                // The fixture is compiled with rustc alone, so keep its JSON protocol
                // responses as text; requests are emitted in serde_json's sorted-key order.
                let mut calls = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("api-calls"))
                    .unwrap();
                writeln!(calls, "{}", args.join(" ")).unwrap();
                for key in ["HC_SESSION", "HC_SOCKET"] {
                    let value =
                        std::env::var(key).map_or_else(|_| "None".into(), |v| format!("Some:{v}"));
                    std::fs::write(root.join(format!("api-{key}")), value).unwrap();
                }
                for line in std::io::stdin().lock().lines() {
                    let line = line.unwrap();
                    writeln!(calls, "{line}").unwrap();
                    calls.flush().unwrap();
                    let id = line
                        .split("\"id\":")
                        .nth(1)
                        .unwrap()
                        .split(',')
                        .next()
                        .unwrap();
                    let result = if line.contains("\"method\":\"hello\"") {
                        "{}".into()
                    } else if line.contains("\"method\":\"machine.list\"") {
                        std::fs::read_to_string(root.join("api-machines")).unwrap_or_else(|error| {
                            if error.kind() == std::io::ErrorKind::NotFound {
                                "{\"machines\":[]}".into()
                            } else {
                                panic!("{error}");
                            }
                        })
                    } else if line.contains("\"method\":\"machine.connect\"") {
                        if line.contains("\"machine\":\"slow\"") {
                            thread::sleep(Duration::from_secs(4));
                        }
                        if line.contains("\"machine\":\"rpc-error\"") {
                            println!(
                                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32601,\"message\":\"unsupported method\"}}}}"
                            );
                            std::io::stdout().flush().unwrap();
                            continue;
                        }
                        if line.contains("\"machine\":\"malformed\"") {
                            println!("invalid JSON");
                            std::io::stdout().flush().unwrap();
                            continue;
                        }
                        if line.contains("\"machine\":\"offline\"") {
                            "{\"state\":\"unreachable\",\"why\":\"fixture tunnel offline\"}".into()
                        } else {
                            "{\"state\":\"connected\"}".into()
                        }
                    } else {
                        panic!("unexpected API request: {line}");
                    };
                    if writeln!(
                        std::io::stdout(),
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}"
                    )
                    .is_err()
                    {
                        return;
                    }
                    if std::io::stdout().flush().is_err() {
                        return;
                    }
                }
            }
            let mut input = std::io::stdin().lock();
            let mut output = std::io::stdout().lock();
            let mut buffer = [0; 8192];
            loop {
                let count = input.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).unwrap();
                output.flush().unwrap();
            }
            eprintln!("fixture hc diagnostic");
            if std::env::var_os("HERDR_TEST_FIXTURE_HANG_AFTER_EOF").is_some() {
                loop {
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
        Some("connect") => {
            std::fs::write(root.join("connect-pid"), std::process::id().to_string()).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let local = listener.local_addr().unwrap().port();
            println!("\x1b[32mForwarding from 127.0.0.1:{local} to host port {port}.\x1b[0m");
            println!("PortForwardingService listening on 127.0.0.1:{local}.");
            std::io::stdout().flush().unwrap();
            for socket in listener.incoming() {
                let mut local = socket.unwrap();
                let mut remote = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
                let mut local_read = local.try_clone().unwrap();
                let mut remote_write = remote.try_clone().unwrap();
                thread::spawn(move || {
                    let _ = std::io::copy(&mut local_read, &mut remote_write);
                    let _ = remote_write.shutdown(Shutdown::Write);
                });
                thread::spawn(move || {
                    let _ = std::io::copy(&mut remote, &mut local);
                    let _ = local.shutdown(Shutdown::Write);
                });
            }
        }
        Some("host") => {
            std::fs::write(root.join("host-pid"), std::process::id().to_string()).unwrap();
            let attempts = root.join("host-attempts");
            let attempt = match std::fs::read_to_string(&attempts) {
                Ok(value) => value.parse::<u32>().unwrap() + 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
                Err(error) => panic!("{error}"),
            };
            std::fs::write(attempts, attempt.to_string()).unwrap();
            let mode = std::env::var("HERDR_TEST_FIXTURE_HOST_MODE").unwrap_or_default();
            if mode != "never-ready" && !(mode == "no-ready-once" && attempt == 1) {
                println!("Ready to accept connections for tunnel: fixture.{cluster}");
                std::io::stdout().flush().unwrap();
            }
            loop {
                let trigger = root.join("host-fail");
                if trigger.exists() {
                    let failure = std::fs::read_to_string(&trigger).unwrap();
                    std::fs::remove_file(trigger).unwrap();
                    match failure.as_str() {
                        "exit" => std::process::exit(1),
                        "success-exit" => std::process::exit(0),
                        "auth" => eprintln!("Refreshed tunnel access token is not valid"),
                        "custom" => println!("fixture tunnel is unhealthy"),
                        _ => panic!("unknown failure trigger"),
                    }
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        _ => panic!("unexpected fixture arguments: {args:?}"),
    }
}
