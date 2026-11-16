pub(crate) fn tunnel_not_found(output: &std::process::Output, tunnel: &str) -> bool {
    if output.status.code() != Some(2) || !output.stdout.is_empty() {
        return false;
    }
    let Ok(stderr) = std::str::from_utf8(&output.stderr) else {
        return false;
    };
    let message = stderr
        .strip_suffix("\r\n")
        .or_else(|| stderr.strip_suffix('\n'))
        .unwrap_or(stderr);
    let valid_name = |name: &str| {
        (1..=151).contains(&name.len())
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    };
    match tunnel.split_once('.') {
        Some((name, cluster))
            if valid_name(name)
                && !cluster.is_empty()
                && cluster.bytes().all(|c| c.is_ascii_alphanumeric()) =>
        {
            message == format!("Tunnel not found in {cluster}: {name}")
        }
        None if valid_name(tunnel) => message == format!("Tunnel not found: {tunnel}"),
        _ => false,
    }
}
