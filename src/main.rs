mod dt;
mod herdr;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hd",
    version,
    about = "Connect Herdr cores through private Dev Tunnels"
)]
struct Arguments {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Herdr(herdr::Arguments),
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    match Arguments::parse().command {
        Commands::Herdr(args) => args.command.run(),
    }
}
