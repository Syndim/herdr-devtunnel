use super::*;
use crate::Arguments as Cli;
use clap::Parser;

fn bootstrap(session: &str) -> String {
    format!(
        "sh -c 'p=$(command -v herdr-core) || p=$HOME/.local/bin/herdr-core; [ -x \"$p\" ] || {{ echo \"herdr-core is not installed here\" >&2; exit 127; }}; {session}exec \"$p\" connect'"
    )
}

#[test]
fn bootstrap_is_connection_only_and_session_bound() {
    validate_bootstrap(&bootstrap(""), "default").unwrap();
    validate_bootstrap(&bootstrap("HC_SESSION=work "), "work").unwrap();
    for line in [
        "uname -sm".into(),
        "cmd /c echo test".into(),
        "hc connect".into(),
        format!("{}; echo unsafe", bootstrap("")),
        bootstrap("HC_SESSION=other "),
    ] {
        assert!(validate_bootstrap(&line, "default").is_err(), "{line}");
    }
    assert!(validate_bootstrap(&bootstrap(""), "work").is_err());
}

#[test]
fn tunnel_policy_rejects_extra_ports_and_grants() {
    let private = json!({"tunnel": {
        "ports": [{"portNumber": 43821}], "accessControl": []
    }});
    validate_tunnel(&private, 43821).unwrap();
    for empty in [
        json!({"ports": []}),
        json!({"tunnelId": "fixture"}),
        json!({"tunnelId": "fixture", "ports": null}),
    ] {
        validate_tunnel(&empty, 43821).unwrap();
    }
    for value in [
        json!({}),
        json!({"ports": [], "accessControl": ["anonymous"]}),
        json!({"ports": "unknown"}),
        json!({"ports": [{"portNumber": 22}]}),
        json!({"ports": [{"portNumber": 43821}, {"portNumber": 22}]}),
        json!({"ports": [{"portNumber": 43821}], "accessControl": ["anonymous"]}),
        json!({"ports": [{"portNumber": 43821, "accessControl": {"entries": ["other-user"]}}]}),
        json!({"ports": [{"portNumber": 43821}], "accessControl": {}}),
    ] {
        assert!(validate_tunnel(&value, 43821).is_err(), "{value}");
    }
    assert!(empty_acl(&json!({"entries": []})).unwrap());
}

#[test]
fn port_list_accepts_only_recognized_responses() {
    for value in [
        json!({"ports": []}),
        json!({"warning": "No ports found for tunnel."}),
    ] {
        assert!(port_list(&value).unwrap().is_empty());
    }
    let list = json!({"ports": [{"portNumber": 43821, "protocol": "auto"}]});
    validate_ports(port_list(&list).unwrap(), 43821).unwrap();
    for value in [
        json!({}),
        json!(null),
        json!([]),
        json!({"ports": null}),
        json!({"ports": {}}),
        json!({"warning": "other"}),
        json!({"warning": "No ports found for tunnel.", "ports": []}),
        json!({"warning": "No ports found for tunnel.", "error": "failed"}),
        json!({"ports": [], "continuationToken": "more"}),
    ] {
        assert!(port_list(&value).is_err(), "{value}");
    }
    for ports in [
        json!([{}]),
        json!([{"portNumber": "43821"}]),
        json!([{"portNumber": 0}]),
        json!([{"portNumber": 43821}, {"portNumber": 22}]),
        json!([{"portNumber": 43821, "accessControl": ["anonymous"]}]),
    ] {
        assert!(validate_ports(ports.as_array().unwrap(), 43821).is_err());
    }
}

#[test]
fn tunnel_ids_accept_split_and_qualified_cli_metadata() {
    for value in [
        json!({"tunnelId": "box", "clusterId": "inc1"}),
        json!({"tunnel": {"tunnelId": "box.inc1"}}),
        json!({"tunnelId": "box.inc1", "clusterId": "inc1"}),
    ] {
        assert_eq!(tunnel_id(&value, "box"), "box.inc1");
    }
}

#[test]
fn forwarding_requires_matching_ipv4_port() {
    assert_eq!(
        forwarding_port(
            "SSH: Forwarding from 127.0.0.1:53123 to host port 43821.",
            43821
        ),
        Some(53123)
    );
    assert_eq!(
        forwarding_port("Forwarding from [::1]:53123 to host port 43821.", 43821),
        None
    );
    assert_eq!(
        forwarding_port("Forwarding from 127.0.0.1:53123 to host port 22.", 43821),
        None
    );
    assert_eq!(
        forwarding_port("Forwarding from 127.0.0.1:0 to host port 43821.", 43821),
        None
    );
}

#[tokio::test]
async fn handshake_preserves_coalesced_binary_and_bounds_input() {
    let mut bytes = &b"{\"protocol\":\"herdr-devtunnel/1\",\"session\":\"work\"}\n\x00\xffrest"[..];
    let hello = read_hello(&mut bytes, 1).await.unwrap();
    assert_eq!(hello.session, "work");
    assert_eq!(bytes, b"\x00\xffrest");
    let oversized = vec![b'x'; HANDSHAKE_LIMIT + 1];
    assert!(read_hello(&mut &oversized[..], 1).await.is_err());
    assert!(read_hello(&mut &b"incomplete"[..], 1).await.is_err());
}

#[test]
fn cli_help_uses_binary_name() {
    use clap::CommandFactory;

    let mut command = Cli::command();
    assert_eq!(command.get_name(), "hd");
    assert!(command.render_help().to_string().contains("Usage: hd "));
}

#[test]
fn cli_requires_explicit_target_and_valid_identifiers() {
    for args in [
        vec!["hd", "herdr", "server"],
        vec!["hd", "herdr", "client", "--tunnel", "box.use2"],
        vec![
            "hd", "herdr", "server", "--tunnel", "box.use2", "--port", "0",
        ],
        vec![
            "hd",
            "herdr",
            "server",
            "--tunnel",
            "box.use2",
            "--session",
            "../other",
        ],
        vec![
            "hd",
            "herdr",
            "client",
            "--tunnel",
            "box.use2",
            "--name",
            "box",
            "--local-session",
            "../other",
        ],
        vec![
            "hd",
            "herdr",
            "reconnect",
            "--all",
            "--local-session",
            "../other",
        ],
        vec![
            "hd",
            "herdr",
            "reconnect",
            "--all",
            "--session",
            "one",
            "--local-session",
            "two",
        ],
        vec![
            "hd",
            "herdr",
            "server",
            "--tunnel",
            "box.use2",
            "--allow-anonymous",
        ],
        vec!["hd", "herdr", "server", "--tunnel", "box.use2", "--create"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
    }
    Cli::try_parse_from(["hd", "herdr", "serve", "--tunnel", "box.use2"]).unwrap();
    Cli::try_parse_from([
        "hd", "herdr", "add", "--name", "box", "--tunnel", "box.use2",
    ])
    .unwrap();
    assert_eq!(shell_quote("a'b"), "'a'\\''b'");
}

#[test]
fn client_and_reconnect_help_describe_sessions_and_transport() {
    use clap::CommandFactory;

    let mut command = Cli::command();
    let herdr = command.find_subcommand_mut("herdr").unwrap();
    let client = herdr
        .find_subcommand_mut("client")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(
        client.contains("Server hc session (the remote session for client registration)"),
        "{client}"
    );
    assert!(client.contains("--local-session"), "{client}");
    assert!(
        client.contains("sets HC_SESSION and removes inherited HC_SOCKET"),
        "{client}"
    );
    assert!(
        client.contains("If omitted, inherits the environment"),
        "{client}"
    );
    let reconnect = herdr
        .find_subcommand_mut("reconnect")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(reconnect.contains("--local-session"), "{reconnect}");
    assert!(
        !reconnect.contains("POSIX quoting currently emitted"),
        "{reconnect}"
    );
}

#[cfg(windows)]
#[test]
fn windows_via_round_trips_through_command_line_to_argv() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    let mut args = vec!["".to_owned(), "測試🦀 &'\t".to_owned()];
    for count in 0..8 {
        let slashes = "\\".repeat(count);
        args.push(format!("x{slashes}"));
        args.push(format!("x{slashes}\"quoted{slashes}"));
    }
    let line = format!(
        "\"program.exe\" {}",
        args.iter()
            .map(|arg| windows_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let wide: Vec<u16> = std::ffi::OsStr::new(&line)
        .encode_wide()
        .chain([0])
        .collect();
    unsafe {
        let mut count = 0;
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
                    .into_string()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(LocalFree(argv.cast()).is_null());
        assert_eq!(&words[1..], args);
    }
}

#[test]
fn windows_via_quotes_backslashes_quotes_and_unicode() {
    for (input, expected) in [
        ("", "\"\""),
        ("plain", "\"plain\""),
        ("a b\tc", "\"a b\tc\""),
        ("a'b & %PATH%", "\"a'b & %PATH%\""),
        (
            r"C:\測試\🦀 folder\app.exe",
            r#""C:\測試\🦀 folder\app.exe""#,
        ),
        ("a\\", "\"a\\\\\""),
        ("a\\\\", "\"a\\\\\\\\\""),
        ("a\"b", "\"a\\\"b\""),
        ("a\\\"b", "\"a\\\\\\\"b\""),
        ("a\\\\\"b", "\"a\\\\\\\\\\\"b\""),
    ] {
        assert_eq!(windows_quote(input), expected, "{input:?}");
    }
}
