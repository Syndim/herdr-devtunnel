pub(crate) mod background;

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};

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

const DEFAULT_FAILURE_PATTERNS: [&str; 4] = [
    r"Refreshed tunnel access token is not valid",
    r"Error connecting (?:host|client) tunnel session: Not authorized",
    r"Error connecting client tunnel session: The tunnel or port is not found \(404\)",
    r"Tunnel service response status code: Unauthorized",
];

pub(crate) fn build_failure_matchers(extra: &[String]) -> Result<Vec<Regex>> {
    let mut matchers = Vec::new();
    for pattern in DEFAULT_FAILURE_PATTERNS
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
    {
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("Invalid failure pattern regex: {pattern}"))?;
        matchers.push(re);
    }
    Ok(matchers)
}

pub(crate) fn line_indicates_failure(matchers: &[Regex], line: &str) -> bool {
    matchers.iter().any(|m| m.is_match(line))
}
