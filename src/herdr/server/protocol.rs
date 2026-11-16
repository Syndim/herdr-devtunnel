use std::time::Duration;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

pub(super) const MAGIC: &str = "herdr-devtunnel/1";
pub(crate) const HANDSHAKE_LIMIT: usize = 4096;
pub(crate) fn validate_bootstrap(line: &str, session: &str) -> Result<()> {
    let pattern = Regex::new(
        r#"^sh -c 'p=\$\(command -v herdr-core\) \|\| p=\$HOME/\.local/bin/herdr-core; \[ -x "\$p" \] \|\| \{ echo "herdr-core is not installed here" >&2; exit 127; \}; (?:HC_SESSION=([a-zA-Z0-9_.-]+) )?exec "\$p" connect'$"#,
    )?;
    let captures = pattern.captures(line).context(
        "Unsupported hc bootstrap (tested: hc 0.1.17). Install/update hc manually; no supplied command was executed"
    )?;
    ensure!(
        captures.get(1).map_or("default", |v| v.as_str()) == session,
        "hc session does not match the bridge session"
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Hello {
    pub(crate) protocol: String,
    pub(crate) session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) ok: Option<bool>,
}

pub(crate) async fn read_line(
    reader: &mut (impl AsyncRead + Unpin),
    limit: usize,
) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let byte = reader
            .read_u8()
            .await
            .context("Stream closed before a complete line")?;
        if byte == b'\n' {
            return Ok(line);
        }
        ensure!(line.len() < limit, "Line exceeds {limit} bytes");
        line.push(byte);
    }
}

pub(crate) async fn read_hello(
    reader: &mut (impl AsyncRead + Unpin),
    seconds: u64,
) -> Result<Hello> {
    let bytes = timeout(
        Duration::from_secs(seconds),
        read_line(reader, HANDSHAKE_LIMIT),
    )
    .await
    .context("Bridge handshake timed out")??;
    serde_json::from_slice(&bytes).context("Invalid bridge handshake")
}

pub(super) async fn write_hello(
    writer: &mut (impl AsyncWrite + Unpin),
    session: &str,
    ok: Option<bool>,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(&Hello {
        protocol: MAGIC.into(),
        session: session.into(),
        ok,
    })?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    Ok(())
}
