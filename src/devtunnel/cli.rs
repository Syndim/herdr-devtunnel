use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};

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
