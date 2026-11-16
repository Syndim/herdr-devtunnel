use anyhow::{Context, Result};
use std::path::PathBuf;

pub(crate) fn identifier(value: &str, max: usize, punctuation: &str) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || punctuation.contains(c))
}

pub(crate) fn parse_session(value: &str) -> std::result::Result<String, String> {
    if identifier(value, 64, "_.-") {
        Ok(value.into())
    } else {
        Err("Use a session of 1-64 ASCII letters/digits, underscores, dots or hyphens".into())
    }
}

pub(crate) fn parse_machine(value: &str) -> std::result::Result<String, String> {
    if identifier(value, 64, "_-") {
        Ok(value.into())
    } else {
        Err("Use a machine name of 1-64 ASCII letters/digits, underscores or hyphens".into())
    }
}

pub(crate) fn parse_tunnel(value: &str) -> std::result::Result<String, String> {
    if value.len() >= 2 && identifier(value, 151, ".-") {
        Ok(value.into())
    } else {
        Err("Use an explicit tunnel name or NAME.CLUSTER identifier".into())
    }
}

pub(crate) fn executable(path: &PathBuf) -> Result<PathBuf> {
    which::which(path).with_context(|| format!("Executable not found: {}", path.display()))
}

#[cfg(any(not(windows), test))]
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(windows, test))]
pub(crate) fn windows_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                quoted.push('"');
                slashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', slashes));
                quoted.push(ch);
                slashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn via_path(path: PathBuf) -> Result<String> {
    path.into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("hc --via requires UTF-8 executable paths"))
}

#[cfg(not(windows))]
pub(crate) fn client_shell() -> Result<PathBuf> {
    which::which("sh").context("hc --via needs sh on PATH")
}
