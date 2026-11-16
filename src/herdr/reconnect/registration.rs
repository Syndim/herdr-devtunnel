use super::{Arguments, api};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::json;

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

pub(super) async fn reconnect_clients(args: &Arguments) -> Result<()> {
    let machines: Machines =
        serde_json::from_value(api::call_local(args, "machine.list", json!({})).await?)
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
                api::call_local(args, "machine.connect", json!({"machine": machine.name})).await?;
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

use std::path::Path;

use crate::herdr::validation::{parse_session, parse_tunnel, shell_quote};

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
        .map(|s| shell_quote(s))
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

pub(super) fn is_our_transport(target: &str, current_exe: &Path) -> bool {
    #[cfg(not(windows))]
    let words = unix_transport_words(target);
    #[cfg(windows)]
    let words = windows_transport_words(target);
    words.is_some_and(|words| is_our_transport_words(&words, current_exe))
}

fn is_our_transport_words(words: &[String], current_exe: &Path) -> bool {
    let Some((executable, args)) = words.split_first() else {
        return false;
    };
    if args.len() < 3 || args[..2] != ["connect", "--tunnel"] || parse_tunnel(&args[2]).is_err() {
        return false;
    }
    let mut seen = 0u8;
    for pair in args[3..].chunks(2) {
        if pair.len() != 2 {
            return false;
        }
        let (bit, valid) = match pair[0].as_str() {
            "--port" => (1, pair[1].parse::<u16>().is_ok_and(|n| n > 0)),
            "--session" => (2, parse_session(&pair[1]).is_ok()),
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
    let path = Path::new(executable);
    if !path.is_absolute() || !same_executable(path, current_exe) {
        eprintln!(
            "[herdr] Skipping transport: executable {} is missing or does not resolve to the current executable",
            path.display()
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unix_parser_accepts_only_our_single_quoted_command() {
        let words = ["/tmp/it's here/hd", "connect", "--tunnel", "abc.use2"];
        let target = words
            .iter()
            .map(|w| super::shell_quote(w))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(unix_transport_words(&target).unwrap(), words);
        let legacy = target.replacen(" 'connect'", " 'herdr' 'connect'", 1);
        assert_eq!(
            unix_transport_words(&legacy).unwrap(),
            [
                "/tmp/it's here/hd",
                "herdr",
                "connect",
                "--tunnel",
                "abc.use2"
            ]
        );
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
        for (command, expected) in [
            (
                r#"  "C:\Program Files\hd.exe" connect --tunnel "test.use2""#,
                vec![
                    r"C:\Program Files\hd.exe",
                    "connect",
                    "--tunnel",
                    "test.use2",
                ],
            ),
            (
                r#"  "C:\Program Files\hd.exe" herdr connect --tunnel "test.use2""#,
                vec![
                    r"C:\Program Files\hd.exe",
                    "herdr",
                    "connect",
                    "--tunnel",
                    "test.use2",
                ],
            ),
        ] {
            assert_eq!(windows_transport_words(command).unwrap(), expected);
        }
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
    fn transport_recognition_validates_command_form() {
        let exe = std::env::current_exe().unwrap();
        let exe_str = exe.to_str().unwrap();
        let prefix = [exe_str];
        for (args, expected) in [
            (vec!["connect", "--tunnel", "abc.use2"], true),
            (
                vec![
                    "connect",
                    "--tunnel",
                    "abc.use2",
                    "--timeout",
                    "600",
                    "--port",
                    "65535",
                    "--session",
                    "work",
                    "--devtunnel",
                    "path with spaces",
                ],
                true,
            ),
            (vec!["connect", "--tunnel", "a"], false),
            (vec!["connect", "--tunnel", "bad;command"], false),
            (vec!["connect", "--tunnel"], false),
            (vec!["connect", "--tunnel=abc.use2"], false),
            (
                vec!["connect", "--tunnel", "abc.use2", "--tunnel", "other"],
                false,
            ),
            (
                vec![
                    "connect", "--tunnel", "abc.use2", "--port", "1", "--port", "2",
                ],
                false,
            ),
            (
                vec![
                    "connect",
                    "--tunnel",
                    "abc.use2",
                    "--session",
                    "one",
                    "--session",
                    "two",
                ],
                false,
            ),
            (
                vec![
                    "connect",
                    "--tunnel",
                    "abc.use2",
                    "--timeout",
                    "1",
                    "--timeout",
                    "2",
                ],
                false,
            ),
            (
                vec![
                    "connect",
                    "--tunnel",
                    "abc.use2",
                    "--devtunnel",
                    "one",
                    "--devtunnel",
                    "two",
                ],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--port", "0"],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--port", "65536"],
                false,
            ),
            (
                vec![
                    "connect",
                    "--tunnel",
                    "abc.use2",
                    "--session",
                    "bad session",
                ],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--timeout", "0"],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--timeout", "601"],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--devtunnel", ""],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--hc", "evil"],
                false,
            ),
            (
                vec!["connect", "--tunnel", "abc.use2", "--bootstrap", "evil"],
                false,
            ),
            (vec!["connect", "--tunnel", "abc.use2", "--session"], false),
            (vec!["connect", "--tunnel", "abc.use2", "extra"], false),
            (vec!["connect", "connect", "--tunnel", "abc.use2"], false),
            (vec!["herdr", "connect", "--tunnel", "abc.use2"], false),
            (vec!["client", "--tunnel", "abc.use2"], false),
        ] {
            let words = prefix.iter().copied().chain(args).collect::<Vec<_>>();
            let unix = words
                .iter()
                .map(|word| super::shell_quote(word))
                .collect::<Vec<_>>()
                .join(" ");
            let windows = words
                .iter()
                .map(|word| crate::herdr::validation::windows_quote(word))
                .collect::<Vec<_>>()
                .join(" ");
            #[cfg(not(windows))]
            let native = &unix;
            #[cfg(windows)]
            let native = &windows;
            assert_eq!(is_our_transport(native, &exe), expected, "{words:?}");
            for parsed in [
                unix_transport_words(&unix),
                windows_transport_words(&windows),
            ] {
                assert_eq!(
                    is_our_transport_words(&parsed.unwrap(), &exe),
                    expected,
                    "{words:?}"
                );
            }
        }
        for path in ["hd", "", "missing"] {
            let words = [path, "connect", "--tunnel", "abc.use2"].map(str::to_string);
            assert!(!is_our_transport_words(&words, &exe), "{path:?}");
        }
        assert!(!is_our_transport_words(&[], &exe));
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
