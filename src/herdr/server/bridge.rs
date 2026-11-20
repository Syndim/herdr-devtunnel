use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::timeout;

use super::protocol::{MAGIC, read_hello, write_hello};
use super::supervisor::{BridgeActivity, stopped};
use crate::process::{combine_results, command};

const BRIDGE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const STDERR_LINE_LIMIT: usize = 65536;

async fn log_child_stderr(mut stream: impl AsyncRead + Unpin, id: u64) {
    let mut chunk = [0; 8192];
    let mut line = Vec::new();
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(count) => {
                for &byte in &chunk[..count] {
                    if byte == b'\n' {
                        log_child_line(id, &line);
                        line.clear();
                    } else {
                        line.push(byte);
                        if line.len() == STDERR_LINE_LIMIT {
                            let split = match std::str::from_utf8(&line) {
                                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                                _ => line.len(),
                            };
                            log_child_line(id, &line[..split]);
                            line.drain(..split);
                        }
                    }
                }
            }
            Err(error) => {
                log::warn!(target: "herdr_events", "[herdr] Bridge #{id} hc stderr read failed: {error}");
                break;
            }
        }
    }
    if !line.is_empty() {
        log_child_line(id, &line);
    }
}

fn log_child_line(id: u64, bytes: &[u8]) {
    let line = String::from_utf8_lossy(bytes);
    log::info!(target: "herdr_events", "[herdr] Bridge #{id} hc: {}", line.trim_end_matches('\r'));
}

#[derive(Debug)]
struct WriteProgressTimeout(Duration);

impl std::fmt::Display for WriteProgressTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bridge write made no progress for {:?}", self.0)
    }
}

impl std::error::Error for WriteProgressTimeout {}

fn is_write_timeout(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<WriteProgressTimeout>())
}

async fn with_write_deadline<T>(
    duration: Duration,
    operation: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    if duration.is_zero() {
        return operation.await;
    }
    timeout(duration, operation)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, WriteProgressTimeout(duration)))?
}

async fn forward(
    source: &mut (impl AsyncRead + Unpin),
    destination: &mut (impl AsyncWrite + Unpin),
    write_timeout: Duration,
) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let count = source.read(&mut buffer).await?;
        if count == 0 {
            return destination.shutdown().await;
        }
        let mut written = 0;
        while written < count {
            let n = with_write_deadline(write_timeout, destination.write(&buffer[written..count]))
                .await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "bridge write returned zero",
                ));
            }
            written += n;
        }
        with_write_deadline(write_timeout, destination.flush()).await?;
    }
}

pub(super) async fn bridge(
    mut socket: TcpStream,
    hc: PathBuf,
    session: String,
    activity: &BridgeActivity,
    mut stop: watch::Receiver<bool>,
    write_timeout: Duration,
) -> (Result<()>, Result<()>) {
    let mut child = None;
    let mut stderr_task = None;
    let mut reason = "handshake or setup failed";
    let result = async {
        socket.set_nodelay(true)?;
        let hello = tokio::select! {
            biased;
            result = stopped(&mut stop) => {
                reason = "server shutdown";
                return result;
            },
            result = read_hello(&mut socket, 10) => result?,
        };
        ensure!(
            hello.protocol == MAGIC && hello.session == session,
            "Wrong protocol or session"
        );
        activity.set_stage("setup");
        if *stop.borrow() {
            reason = "server shutdown";
            return Ok(());
        }
        child = Some(
            command(hc)
                .env_remove("HC_SOCKET")
                .args(["connect", "--session", &session])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let child = child.as_mut().expect("hc was just spawned");
        let mut input = child.stdin.take().context("Missing hc stdin")?;
        let mut output = child.stdout.take().context("Missing hc stdout")?;
        let stderr = child.stderr.take().context("Missing hc stderr")?;
        stderr_task = Some(tokio::spawn(log_child_stderr(stderr, activity.id())));
        tokio::select! {
            biased;
            result = stopped(&mut stop) => {
                reason = "server shutdown";
                return result;
            },
            result = write_hello(&mut socket, &session, Some(true)) => result?,
        }
        activity.set_stage("forwarding");
        reason = "forwarding failed";
        let (mut read, mut write) = socket.into_split();
        let incoming = async {
            let result = forward(&mut read, &mut input, write_timeout).await;
            drop(input);
            result
        };
        let outgoing = forward(&mut output, &mut write, write_timeout);
        // Preserve half-close responses without letting hc retain a bridge slot
        // forever after client EOF. Output EOF still ends an open-input peer.
        tokio::pin!(incoming);
        tokio::pin!(outgoing);
        tokio::select! {
            biased;
            result = stopped(&mut stop) => {
                reason = "server shutdown";
                result
            },
            result = &mut incoming => {
                reason = if result.as_ref().is_err_and(is_write_timeout) {
                    "hc input write timed out"
                } else {
                    "client input failed"
                };
                result?;
                reason = "client EOF";
                activity.set_stage("draining");
                tokio::select! {
                    biased;
                    result = stopped(&mut stop) => {
                        reason = "server shutdown";
                        result
                    },
                    result = timeout(BRIDGE_DRAIN_TIMEOUT, outgoing) => {
                        reason = "client EOF; hc output drain";
                        let result = result.context("hc output drain timed out after client EOF")?;
                        if result.as_ref().is_err_and(is_write_timeout) {
                            reason = "hc output write timed out";
                        }
                        result.map_err(Into::into)
                    }
                }
            }
            result = &mut outgoing => {
                reason = match &result {
                    Ok(()) => "hc output closed",
                    Err(error) if is_write_timeout(error) => "hc output write timed out",
                    Err(_) => "hc output failed",
                };
                result.map_err(Into::into)
            },
        }
    }
    .await;
    activity.set_stage("cleanup");
    let cleanup_deadline = tokio::time::Instant::now() + BRIDGE_CLEANUP_TIMEOUT;
    let cleanup = if let Some(mut child) = child {
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            status => {
                let kill = child
                    .start_kill()
                    .context("Failed to stop hc bridge process");
                let wait = tokio::time::timeout_at(cleanup_deadline, child.wait())
                    .await
                    .context("Timed out waiting for hc bridge process to exit")
                    .and_then(|result| result.context("Failed to reap hc bridge process"));
                match wait {
                    Ok(_) => Ok(()),
                    Err(error) => combine_results(
                        combine_results(
                            status
                                .map(|_| ())
                                .context("Failed to check hc bridge process"),
                            kill,
                        ),
                        Err(error),
                    ),
                }
            }
        }
    } else {
        Ok(())
    };
    if let Some(mut task) = stderr_task {
        if tokio::time::timeout_at(cleanup_deadline, &mut task)
            .await
            .is_err()
        {
            task.abort();
            log::warn!(target: "herdr_events",
                "[herdr] Bridge #{} hc stderr drain timed out",
                activity.id()
            );
        }
    }
    log::info!(target: "herdr_events",
        "[herdr] Bridge #{} closed: reason={reason}; age={}s; result={}; cleanup={}",
        activity.id(),
        activity.age().as_secs(),
        if result.is_ok() { "ok" } else { "error" },
        if cleanup.is_ok() { "ok" } else { "error" }
    );
    (result, cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};
    use tokio::io::AsyncWrite;
    use tokio::time::sleep;

    #[tokio::test]
    async fn blocked_output_times_out() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let error = forward(&mut &b"ab"[..], &mut writer, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(is_write_timeout(&error));
        assert!(!is_write_timeout(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(error.to_string().contains("bridge write made no progress"));
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_flush_times_out_even_when_source_goes_idle() {
        let (mut source, mut input) = tokio::io::duplex(8);
        let (writer, _reader) = tokio::io::duplex(1);
        let mut writer = tokio::io::BufWriter::with_capacity(8, writer);
        source.write_all(b"ab").await.unwrap();
        let error = forward(&mut input, &mut writer, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(is_write_timeout(&error));
    }

    #[tokio::test]
    async fn idle_reads_are_not_timed_and_eof_shuts_down_socket() {
        let (mut hc, mut output) = tokio::io::duplex(16);
        let (mut socket, mut peer) = tokio::io::duplex(16);
        let task = tokio::spawn(async move {
            forward(&mut output, &mut socket, Duration::from_millis(50)).await
        });
        sleep(Duration::from_millis(120)).await;
        assert!(!task.is_finished());
        hc.write_all(b"\0\xfffinal").await.unwrap();
        hc.shutdown().await.unwrap();
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(2), peer.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"\0\xfffinal");
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn partial_writes_reset_the_timeout() {
        use std::future::{Future, poll_fn};

        let (mut socket, mut peer) = tokio::io::duplex(1);
        let mut output = &b"abcd"[..];
        let write_timeout = Duration::from_millis(500);
        let start = tokio::time::Instant::now();
        let task = forward(&mut output, &mut socket, write_timeout);
        tokio::pin!(task);

        assert!(
            poll_fn(|cx| Poll::Ready(task.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        let mut received = Vec::new();
        for i in 0..3 {
            tokio::time::advance(Duration::from_millis(300)).await;
            let mut byte = [0];
            peer.read_exact(&mut byte).await.unwrap();
            received.push(byte[0]);
            let result = poll_fn(|cx| Poll::Ready(task.as_mut().poll(cx))).await;
            if i < 2 {
                assert!(result.is_pending());
            } else {
                assert!(start.elapsed() > write_timeout);
                assert!(matches!(result, Poll::Ready(Ok(()))));
            }
        }
        let mut byte = [0];
        peer.read_exact(&mut byte).await.unwrap();
        received.push(byte[0]);
        assert_eq!(received, b"abcd");
    }

    #[tokio::test]
    async fn zero_timeout_disables_write_deadline() {
        let (mut socket, mut peer) = tokio::io::duplex(1);
        let task =
            tokio::spawn(
                async move { forward(&mut &b"ab"[..], &mut socket, Duration::ZERO).await },
            );
        sleep(Duration::from_millis(120)).await;
        assert!(!task.is_finished());
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(2), peer.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"ab");
        task.await.unwrap().unwrap();
    }

    struct FailingWriter(io::ErrorKind);

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.0 == io::ErrorKind::WriteZero {
                Poll::Ready(Ok(0))
            } else {
                Poll::Ready(Err(io::Error::from(self.0)))
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn output_write_errors_are_propagated() {
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::WriteZero] {
            let error = forward(
                &mut &b"x"[..],
                &mut FailingWriter(kind),
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), kind);
        }
    }
}
