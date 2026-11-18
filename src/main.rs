mod agent;
mod background;
mod cli;
mod devtunnel;
mod diagnostics;
mod herdr;
mod process;

use anyhow::Result;
use std::io::Write;

fn main() -> Result<()> {
    let filters = std::env::var("RUST_LOG").unwrap_or_default();
    env_logger::Builder::new()
        .parse_filters(filters.split('/').next().unwrap_or_default())
        .filter_level(log::LevelFilter::Info)
        .filter_module("herdr_events", log::LevelFilter::Info)
        .format(|buf, record| {
            let message = record.args().to_string();
            for line in message.split('\n') {
                writeln!(buf, "[{}] {}", buf.timestamp_millis(), line)?;
            }
            Ok(())
        })
        .init();
    if let Err(error) = cli::run() {
        log::error!(target: "herdr_events", "Error: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}
