#[cfg(not(test))]
fn main() {
    use std::io::{BufRead, Write};
    let root = std::path::PathBuf::from(std::env::var_os("RECONNECT_TEST_ROOT").unwrap());
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("calls"))
        .unwrap();
    writeln!(
        log,
        "args: {}",
        std::env::args().skip(1).collect::<Vec<_>>().join(" ")
    )
    .unwrap();
    writeln!(log, "socket: {:?}", std::env::var_os("HC_SOCKET")).unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        writeln!(log, "{line}").unwrap();
        log.flush().unwrap();
        let id = line
            .split("\"id\":")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();
        let result = if line.contains("\"method\":\"machine.list\"") {
            std::fs::read_to_string(root.join("machines")).unwrap()
        } else if line.contains("\"method\":\"machine.connect\"") {
            "{\"state\":\"connected\"}".to_string()
        } else {
            "{}".to_string()
        };
        println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}");
        std::io::stdout().flush().unwrap();
    }
}

#[cfg(test)]
#[test]
fn reconnect_only_our_standalone_transport_and_ignores_inherited_socket() {
    use std::{fs, io::Write, path::Path, process::Command};
    let root = std::env::temp_dir().join(format!("reconnect's test {}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let registered = root.join(format!("registered{}", std::env::consts::EXE_SUFFIX));
    let upgrade = root.join(format!("upgrade{}", std::env::consts::EXE_SUFFIX));
    let copy = root.join(format!("copy{}", std::env::consts::EXE_SUFFIX));
    let hc = root.join(format!("hc{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_hd"), &registered).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&registered)
        .unwrap()
        .write_all(b"old version")
        .unwrap();
    fs::copy(env!("CARGO_BIN_EXE_hd"), &upgrade).unwrap();
    fs::copy(&upgrade, &copy).unwrap();
    let compile = Command::new("rustc")
        .arg("--edition=2024")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/herdr_reconnect.rs"))
        .arg("-o")
        .arg(&hc)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    #[cfg(not(windows))]
    let quote = |arg: &str| format!("'{}'", arg.replace('\'', "'\\''"));
    #[cfg(windows)]
    let quote = |arg: &str| {
        let mut result = String::from("\"");
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                    result.push(c);
                    slashes = 0;
                }
                _ => {
                    result.extend(std::iter::repeat_n('\\', slashes));
                    result.push(c);
                    slashes = 0;
                }
            }
        }
        result.extend(std::iter::repeat_n('\\', slashes * 2));
        result.push('"');
        result
    };
    let target = |path: &Path| {
        [
            path.to_str().unwrap(),
            "herdr",
            "connect",
            "--tunnel",
            "test.use2",
        ]
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
    };
    let valid = target(&registered);
    let full = format!(
        "{} {} {} {} {} {} {} {} {}",
        target(&registered),
        quote("--port"),
        quote("43821"),
        quote("--session"),
        quote("work"),
        quote("--timeout"),
        quote("45"),
        quote("--devtunnel"),
        quote(&hc.to_string_lossy()),
    );
    let bad_suffix = if cfg!(windows) {
        " \"unrelated\""
    } else {
        "; 'echo' 'bad'"
    };
    fs::create_dir(root.join("subdir")).unwrap();
    let normalized = root
        .join("subdir")
        .join("..")
        .join(registered.file_name().unwrap());
    let mut machines = serde_json::json!({"machines": [
        {"name":"ours", "target":valid, "via":true},
        {"name":"complete", "target":full, "via":true},
        {"name":"normalized", "target":target(&normalized), "via":true},
        {"name":"copy", "target":target(&copy), "via":true},
        {"name":"other", "target":target(&hc), "via":true},
        {"name":"missing", "target":target(&root.join("missing")), "via":true},
        {"name":"ssh", "target":target(&registered), "via":false},
        {"name":"unrelated", "target":format!("{}{}", target(&registered), bad_suffix), "via":true},
        {"name":"relative", "target":target(Path::new("hd")), "via":true},
    ]});
    #[cfg(windows)]
    machines["machines"].as_array_mut().unwrap().push(serde_json::json!({
        "name":"escaped", "target":format!("{} {} {}", target(&registered), quote("--devtunnel"), quote("C:\\folder\\\"quoted\\\\")), "via":true
    }));
    #[cfg(unix)]
    {
        let alias = root.join("alias");
        let wrong_alias = root.join("wrong-alias");
        std::os::unix::fs::symlink(&registered, &alias).unwrap();
        std::os::unix::fs::symlink(&copy, &wrong_alias).unwrap();
        machines["machines"].as_array_mut().unwrap().extend([
            serde_json::json!({"name":"symlink", "target":target(&alias), "via":true}),
            serde_json::json!({"name":"other-symlink", "target":target(&wrong_alias), "via":true}),
        ]);
    }
    fs::write(
        root.join("machines"),
        serde_json::to_vec(&machines).unwrap(),
    )
    .unwrap();
    assert_ne!(fs::read(&registered).unwrap(), fs::read(&upgrade).unwrap());
    fs::remove_file(&registered).unwrap();
    fs::rename(&upgrade, &registered).unwrap();
    let output = Command::new(&registered)
        .args([
            "herdr",
            "reconnect",
            "--all",
            "--role",
            "client",
            "--session",
            "wanted",
            "--hc",
        ])
        .arg(&hc)
        .env("HC_SOCKET", root.join("wrong.sock"))
        .env("HC_SESSION", "wrong")
        .env("RECONNECT_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fs::read_to_string(root.join("calls")).unwrap();
    let expected_calls = 5;
    assert_eq!(
        calls.matches("args: connect --session wanted").count(),
        expected_calls
    );
    assert_eq!(calls.matches("socket: None").count(), expected_calls);
    let connected: Vec<_> = calls
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|request| request["method"] == "machine.connect")
        .map(|request| request["params"]["machine"].as_str().unwrap().to_string())
        .collect();
    let expected = if cfg!(unix) {
        vec!["ours", "complete", "normalized", "symlink"]
    } else {
        vec!["ours", "complete", "normalized", "escaped"]
    };
    assert_eq!(connected, expected);
    fs::remove_dir_all(root).unwrap();
}
