use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

use super::{Role, background, command, executable, parse_session};

#[derive(Args)]
pub struct Arguments {
    /// Reconnect all running server profiles and registered Herdr tunnel machines.
    #[arg(
        long,
        required = true,
        long_help = "Reconnect running servers and registered Herdr tunnel machines. Client registrations must use an absolute executable path resolving to this executable, including through symlinks. Replacing the binary at the same path preserves recognition; a different path, even a copy, does not."
    )]
    all: bool,
    /// Limit reconnection to local servers or remote client connections.
    #[arg(long, value_enum, default_value = "both")]
    role: Role,
    /// Local hc session containing the remote machine registrations.
    #[arg(long, visible_alias = "local-session", default_value = "default", value_parser = parse_session)]
    session: String,
    #[arg(long, default_value = "hc")]
    hc: PathBuf,
    /// Maximum seconds for each local hc API call.
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=600))]
    timeout: u64,
}

pub(super) async fn run(args: Arguments) -> Result<()> {
    let mut failures = Vec::new();
    if args.role != Role::Client
        && let Err(error) = background::reconnect_all()
    {
        eprintln!("[herdr] Server reconnect failed: {error:#}");
        failures.push(format!("servers: {error:#}"));
    }
    if args.role != Role::Server
        && let Err(error) = reconnect_clients(&args).await
    {
        eprintln!("[herdr] Client reconnect failed: {error:#}");
        failures.push(format!("clients: {error:#}"));
    }
    ensure!(
        failures.is_empty(),
        "Herdr reconnect incomplete:\n{}",
        failures.join("\n")
    );
    Ok(())
}

#[derive(Deserialize)]
struct Machine {
    name: String,
    target: String,
    #[serde(default)]
    via: bool,
}

#[derive(Deserialize)]
struct Machines {
    machines: Vec<Machine>,
}

async fn reconnect_clients(args: &Arguments) -> Result<()> {
    let machines: Machines =
        serde_json::from_value(call_local(args, "machine.list", json!({})).await?)
            .context("Invalid hc machine list")?;
    let current_exe = std::env::current_exe().context("Cannot identify reconnect executable")?;
    let mut count = 0;
    let mut failures = Vec::new();
    for machine in machines.machines {
        if !machine.via || !is_our_transport(&machine.target, &current_exe) {
            continue;
        }
        count += 1;
        let result = async {
            let result =
                call_local(args, "machine.connect", json!({"machine": machine.name})).await?;
            let state = result["state"]
                .as_str()
                .context("hc machine.connect returned no state")?;
            ensure!(
                state == "connected",
                "state={state}; {}",
                result["why"].as_str().unwrap_or("connection is not ready")
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => println!("Machine '{}' is connected.", machine.name),
            Err(error) => {
                eprintln!("[herdr] Machine '{}': {error:#}", machine.name);
                failures.push(format!("{}: {error:#}", machine.name));
            }
        }
    }
    if count == 0 {
        println!("No registered Herdr dev-tunnel machines found.");
    }
    ensure!(
        failures.is_empty(),
        "Failed to reconnect {} machine(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

#[cfg(any(not(windows), test))]
fn unix_transport_words(target: &str) -> Option<Vec<String>> {
    let mut rest = target;
    let mut words = Vec::new();
    loop {
        rest = rest.strip_prefix('\'')?;
        let mut word = String::new();
        loop {
            let end = rest.find('\'')?;
            word.push_str(&rest[..end]);
            rest = &rest[end + 1..];
            if let Some(after) = rest.strip_prefix("\\''") {
                word.push('\'');
                rest = after;
            } else {
                break;
            }
        }
        words.push(word);
        if rest.is_empty() {
            break;
        }
        rest = rest.strip_prefix(' ')?;
    }
    if words
        .iter()
        .map(|s| super::shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ")
        != target
    {
        return None;
    }
    Some(words)
}

#[cfg(any(windows, test))]
fn windows_transport_words(target: &str) -> Option<Vec<String>> {
    if target.chars().any(char::is_control) {
        return None;
    }
    let mut chars = target.trim_start_matches(' ').chars().peekable();
    let mut executable = String::new();
    if chars.peek() == Some(&'"') {
        chars.next();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            executable.push(c);
        }
    } else {
        while chars.peek().is_some_and(|c| *c != ' ') {
            executable.push(chars.next()?);
        }
    }
    if executable.is_empty() {
        return None;
    }
    let mut words = vec![executable];
    while chars.peek().is_some() {
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut word = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c == ' ' && !quoted {
                break;
            }
            let mut slashes = 0;
            while chars.peek() == Some(&'\\') {
                chars.next();
                slashes += 1;
            }
            if chars.peek() == Some(&'"') {
                word.extend(std::iter::repeat_n('\\', slashes / 2));
                chars.next();
                let mut quotes = usize::from(quoted);
                if slashes % 2 == 1 {
                    word.push('"');
                } else {
                    quotes += 1;
                }
                while chars.peek() == Some(&'"') {
                    chars.next();
                    quotes += 1;
                    if quotes == 3 {
                        word.push('"');
                        quotes = 0;
                    }
                }
                quoted = quotes == 1;
            } else {
                word.extend(std::iter::repeat_n('\\', slashes));
                if let Some(&c) = chars.peek()
                    && (c != ' ' || quoted)
                {
                    word.push(c);
                    chars.next();
                }
            }
        }
        words.push(word);
    }
    Some(words)
}

fn same_executable(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn is_our_transport(target: &str, current_exe: &Path) -> bool {
    #[cfg(not(windows))]
    let words = unix_transport_words(target);
    #[cfg(windows)]
    let words = windows_transport_words(target);
    let Some(words) = words else { return false };
    if words.len() < 5
        || words[1..4] != ["herdr", "connect", "--tunnel"]
        || super::parse_tunnel(&words[4]).is_err()
    {
        return false;
    }
    let mut seen = 0u8;
    for pair in words[5..].chunks(2) {
        if pair.len() != 2 {
            return false;
        }
        let (bit, valid) = match pair[0].as_str() {
            "--port" => (1, pair[1].parse::<u16>().is_ok_and(|n| n > 0)),
            "--session" => (2, super::parse_session(&pair[1]).is_ok()),
            "--timeout" => (
                4,
                pair[1].parse::<u64>().is_ok_and(|n| (1..=600).contains(&n)),
            ),
            "--devtunnel" => (8, !pair[1].is_empty()),
            _ => return false,
        };
        if !valid || seen & bit != 0 {
            return false;
        }
        seen |= bit;
    }
    let path = Path::new(&words[0]);
    if !path.is_absolute() || !same_executable(path, current_exe) {
        eprintln!(
            "[herdr] Skipping transport: executable {} is missing or does not resolve to the current executable",
            path.display()
        );
        return false;
    }
    true
}

async fn call_local(args: &Arguments, method: &str, params: Value) -> Result<Value> {
    // A separate connector prevents a timed-out reply from being mistaken for
    // the next machine's response. Closing it does not stop the persistent core.
    let mut api = Api::open(args)?;
    let result = async {
        api.call("hello", json!({})).await?;
        api.call(method, params).await
    }
    .await;
    let cleanup = api.close().await;
    if result.is_err()
        && let Err(error) = &cleanup
    {
        eprintln!("[herdr] hc API cleanup failed: {error:#}");
    }
    result.and_then(|value| cleanup.map(|()| value))
}

struct Api {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    seconds: u64,
    next_id: u64,
}

impl Api {
    fn open(args: &Arguments) -> Result<Self> {
        let mut child = command(executable(&args.hc)?)
            .args(["connect", "--session", &args.session])
            // hc gives HC_SOCKET priority over --session. Never inherit a socket
            // pin from the caller when selecting a local session explicitly.
            .env_remove("HC_SOCKET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Failed to open the local hc API")?;
        let input = child.stdin.take().context("Missing hc stdin")?;
        let output = BufReader::new(child.stdout.take().context("Missing hc stdout")?);
        Ok(Self {
            child,
            input,
            output,
            seconds: args.timeout,
            next_id: 0,
        })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        timeout(Duration::from_secs(self.seconds), async {
            let mut request = serde_json::to_vec(
                &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            )?;
            request.push(b'\n');
            self.input.write_all(&request).await?;
            self.input.flush().await?;
            loop {
                let mut line = Vec::new();
                let length = (&mut self.output)
                    .take(1024 * 1024 + 1)
                    .read_until(b'\n', &mut line)
                    .await?;
                ensure!(length > 0, "hc API closed while calling {method}");
                ensure!(
                    length <= 1024 * 1024 && line.last() == Some(&b'\n'),
                    "hc API response exceeds 1 MiB or is incomplete"
                );
                let response: Value =
                    serde_json::from_slice(&line).context("Invalid hc API response")?;
                if response.get("id").is_none() && response["method"].is_string() {
                    continue;
                }
                ensure!(response["id"] == id, "Unexpected hc API response ID");
                if let Some(error) = response.get("error") {
                    bail!("hc {method} failed: {error}");
                }
                return response
                    .get("result")
                    .cloned()
                    .context("hc API response has no result");
            }
        })
        .await
        .with_context(|| format!("hc {method} timed out after {} seconds", self.seconds))?
    }

    async fn close(mut self) -> Result<()> {
        self.input.shutdown().await?;
        drop(self.input);
        match timeout(Duration::from_secs(5), self.child.wait()).await {
            Ok(status) => {
                let status = status.context("Failed to reap hc API connector")?;
                ensure!(status.success(), "hc API connector failed: {status}");
                Ok(())
            }
            Err(_) => {
                self.child.kill().await?;
                bail!("hc API connector did not exit after closing stdin")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_parser_accepts_only_our_single_quoted_command() {
        let words = [
            "/tmp/it's here/hd",
            "herdr",
            "connect",
            "--tunnel",
            "abc.use2",
        ];
        let target = words
            .iter()
            .map(|w| super::super::shell_quote(w))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(unix_transport_words(&target).unwrap(), words);
        for suffix in [
            "; 'echo' 'bad'",
            " > /tmp/redirect",
            " 'extra'\n'bad'",
            " ",
            "''",
        ] {
            assert!(
                unix_transport_words(&(target.clone() + suffix)).is_none(),
                "{suffix:?}"
            );
        }
        assert!(unix_transport_words("'unterminated").is_none());
        assert!(unix_transport_words("'path' 'herdr' $(evil)").is_none());
    }

    #[test]
    fn windows_parser_matches_hc_rules_for_quotes_and_backslashes() {
        let words = windows_transport_words(
            r#"  "C:\Program Files\hd.exe" herdr connect --tunnel "test.use2""#,
        )
        .unwrap();
        assert_eq!(
            words,
            [
                r"C:\Program Files\hd.exe",
                "herdr",
                "connect",
                "--tunnel",
                "test.use2"
            ]
        );
        let words =
            windows_transport_words("'C:/Program Files/hd.exe' 'herdr' 'connect' '--tunnel' 'abc'")
                .unwrap();
        assert_ne!(words[1], "herdr");
        for (command, expected) in [
            (
                r#"bridge.exe "C:\folder\\" "a\"b" """#,
                vec!["bridge.exe", "C:\\folder\\", "a\"b", ""],
            ),
            (
                r#"bridge.exe a\\\"b a\\\\"b c"#,
                vec!["bridge.exe", "a\\\"b", "a\\\\b c"],
            ),
            (r#"bridge.exe "a""b""#, vec!["bridge.exe", "a\"b"]),
            (r#"bridge.exe """""#, vec!["bridge.exe", "\""]),
            (r#"bridge.exe """"""#, vec!["bridge.exe", "\""]),
            (r#""C:\dir\" herdr"#, vec!["C:\\dir\\", "herdr"]),
            (
                r#"bridge.exe "unterminated argument"#,
                vec!["bridge.exe", "unterminated argument"],
            ),
            (r#""C:\program.exe"herdr"#, vec!["C:\\program.exe", "herdr"]),
        ] {
            assert_eq!(
                windows_transport_words(command).unwrap(),
                expected,
                "{command:?}"
            );
        }
        assert!(windows_transport_words("x\nherdr").is_none());
        assert!(windows_transport_words(r#""" arg"#).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_parser_matches_command_line_to_argv_w() {
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16;
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        let fragments = ["", "a", " ", "\\", "\"", "\\\\", "\\\""];
        for first in fragments {
            for second in fragments {
                for third in fragments {
                    let target = format!("  \"C:\\program.exe\" {first}{second}{third} tail");
                    let wide: Vec<u16> = target
                        .trim_start_matches(' ')
                        .encode_utf16()
                        .chain([0])
                        .collect();
                    let mut count = 0;
                    let args = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) };
                    assert!(!args.is_null());
                    let expected = unsafe {
                        let words = std::slice::from_raw_parts(args, count as usize)
                            .iter()
                            .map(|&arg| {
                                let mut len = 0;
                                while *arg.add(len) != 0 {
                                    len += 1;
                                }
                                String::from_utf16(std::slice::from_raw_parts(arg, len)).unwrap()
                            })
                            .collect::<Vec<_>>();
                        LocalFree(args.cast());
                        words
                    };
                    assert_eq!(
                        windows_transport_words(&target).unwrap(),
                        expected,
                        "{target:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn executable_identity_follows_paths_not_binary_contents() {
        let dir = std::env::temp_dir().join(format!("reconnect identity {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        let pinned = dir.join("pinned");
        let copy = dir.join("copy");
        let old = dir.join("old");
        std::fs::write(&pinned, b"old binary").unwrap();
        std::fs::write(&old, b"old binary").unwrap();
        assert!(!same_executable(&pinned, &old));
        std::fs::write(&pinned, b"new binary").unwrap();
        std::fs::copy(&pinned, &copy).unwrap();
        assert!(same_executable(&pinned, &pinned));
        assert!(same_executable(
            &pinned,
            &dir.join("subdir").join("..").join("pinned")
        ));
        assert!(!same_executable(&pinned, &copy));
        assert!(!same_executable(&pinned, &old));
        assert!(!same_executable(&pinned, &dir.join("missing")));
        #[cfg(unix)]
        {
            let alias = dir.join("alias");
            let dangling = dir.join("dangling");
            std::os::unix::fs::symlink(&pinned, &alias).unwrap();
            std::os::unix::fs::symlink(dir.join("missing"), &dangling).unwrap();
            assert!(same_executable(&pinned, &alias));
            assert!(!same_executable(&pinned, &dangling));
            std::fs::remove_file(&alias).unwrap();
            std::os::unix::fs::symlink(&copy, &alias).unwrap();
            assert!(!same_executable(&pinned, &alias));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
