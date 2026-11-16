pub(crate) mod lifecycle;
mod profile;
mod reporter;
mod state;
mod worker;

use crate::herdr::validation::parse_machine;
use clap::Args;
use std::path::PathBuf;

pub(crate) use profile::{reconnect_all, start, status, stop};
pub(crate) use reporter::Reporter;
pub(crate) use worker::worker;

#[derive(Args)]
pub struct SessionArgs {
    /// Profile supplied to `hd server --name`.
    #[arg(long, default_value = "default", value_parser = parse_machine)]
    name: String,
}
#[derive(Args)]
pub struct WorkerArgs {
    #[arg(long)]
    directory: PathBuf,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::lifecycle;
    use super::profile::{startup_timeout, stop_in_directory};
    use super::reporter::Reporter;
    use super::state::{State, read_state, write_state};

    #[test]
    fn stop_requires_a_clean_final_state() {
        for outcome in ["stopped", "failed", "missing", "unfinished"] {
            let directory =
                std::env::temp_dir().join(format!("herdr-stop-{:032x}", rand::random::<u128>()));
            fs::create_dir(&directory).unwrap();
            write_state(
                &directory,
                &State {
                    pid: std::process::id(),
                    tunnel: "test".into(),
                    port: 1234,
                    session: "test".into(),
                    ready: true,
                    restarts: 0,
                    message: "ready".into(),
                },
            )
            .unwrap();
            let lock = lifecycle::try_lock(&directory.join("worker.lock"))
                .unwrap()
                .unwrap();
            let worker_directory = directory.clone();
            let worker = thread::spawn(move || {
                let _lock = lock;
                let started = Instant::now();
                while !worker_directory.join("stop").exists() {
                    assert!(started.elapsed() < Duration::from_secs(10));
                    thread::sleep(Duration::from_millis(10));
                }
                let reporter = Reporter {
                    directory: Some(worker_directory.clone()),
                };
                match outcome {
                    "stopped" => reporter.finish(&Ok(())).unwrap(),
                    "failed" => reporter
                        .finish(&Err(anyhow::anyhow!("child cleanup failed")))
                        .unwrap(),
                    "missing" => fs::remove_file(worker_directory.join("state.json")).unwrap(),
                    "unfinished" => {}
                    _ => unreachable!(),
                }
            });
            let result = stop_in_directory("test", &directory);
            worker.join().unwrap();
            match outcome {
                "stopped" => assert!(result.is_ok()),
                "failed" => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("child cleanup failed"))
                }
                "missing" => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("without a final status"))
                }
                "unfinished" => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("did not stop cleanly"))
                }
                _ => unreachable!(),
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn stop_ignores_saved_failures_when_not_running() {
        let directory =
            std::env::temp_dir().join(format!("herdr-stop-idle-{:032x}", rand::random::<u128>()));
        fs::create_dir(&directory).unwrap();
        assert!(stop_in_directory("test", &directory).is_ok());
        write_state(
            &directory,
            &State {
                pid: std::process::id(),
                tunnel: "test".into(),
                port: 1234,
                session: "test".into(),
                ready: false,
                restarts: 0,
                message: "failed: earlier error".into(),
            },
        )
        .unwrap();
        assert!(stop_in_directory("test", &directory).is_ok());
        assert_eq!(
            read_state(&directory).unwrap().unwrap().message,
            "failed: earlier error"
        );
        assert!(!directory.join("stop").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn worker_result_keeps_serve_and_reporter_errors() {
        let directory =
            std::env::temp_dir().join(format!("herdr-finish-{:032x}", rand::random::<u128>()));
        fs::create_dir(&directory).unwrap();
        let reporter = Reporter {
            directory: Some(directory.clone()),
        };
        let result = Err(anyhow::anyhow!("child cleanup failed"));
        let status = reporter.finish(&result);
        let error = format!(
            "{:#}",
            crate::process::combine_results(result, status).unwrap_err()
        );
        assert!(error.contains("child cleanup failed"));
        assert!(error.contains("Missing managed Herdr state"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_budget_covers_provisioning_readiness_and_retries() {
        for (timeout, cooldown, retries) in [(1, 0, 1), (45, 2, 1), (600, 30, 3)] {
            let expected =
                12 * timeout * (u64::from(retries) + 1) + (8 + cooldown) * u64::from(retries) + 60;
            assert_eq!(
                startup_timeout(timeout, cooldown, retries),
                Duration::from_secs(expected)
            );
        }
        assert_eq!(startup_timeout(45, 2, 0), startup_timeout(45, 2, 1));
    }

    #[test]
    fn startup_budget_saturates_without_overflow() {
        assert_eq!(
            startup_timeout(u64::MAX, 0, 1),
            Duration::from_secs(u64::MAX)
        );
        assert_eq!(
            startup_timeout(1, u64::MAX, u32::MAX),
            Duration::from_secs(u64::MAX)
        );
    }
}
