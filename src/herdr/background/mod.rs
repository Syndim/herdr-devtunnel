mod lifecycle;
mod profile;
mod state;
mod worker;

use std::path::PathBuf;

use clap::Args;

use crate::config::parse_machine;

pub(crate) use profile::{reconnect_all, start, status, stop};
pub(crate) use state::Reporter;
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
mod tests;
