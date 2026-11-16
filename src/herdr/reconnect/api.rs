use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

use super::Arguments;
use crate::config::executable;
use crate::process::command;

pub(super) async fn call_local(args: &Arguments, method: &str, params: Value) -> Result<Value> {
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
