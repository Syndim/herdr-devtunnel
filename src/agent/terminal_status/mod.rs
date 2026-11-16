use std::io::Write;

use anyhow::{Context, Result};

pub(super) fn write_terminal(sequence: &[u8]) -> Result<()> {
    let device = if cfg!(windows) {
        r"\\.\CONOUT$"
    } else {
        "/dev/tty"
    };
    std::fs::OpenOptions::new()
        .write(true)
        .open(device)
        .with_context(|| format!("Open Herdr terminal {device}"))?
        .write_all(sequence)
        .context("Write status to Herdr terminal")
}
