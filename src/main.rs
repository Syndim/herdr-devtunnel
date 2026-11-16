mod agent;
mod background;
mod cli;
mod devtunnel;
mod diagnostics;
mod herdr;
mod process;

use anyhow::Result;

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    cli::run()
}
