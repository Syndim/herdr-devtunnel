use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hd",
    version,
    about = "Connect Herdr cores through private Dev Tunnels"
)]
pub(crate) struct Arguments {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    #[command(flatten)]
    Root(crate::herdr::Commands),
    #[command(hide = true)]
    Herdr(crate::herdr::Arguments),
}

pub(crate) fn run() -> Result<()> {
    use clap::Parser;
    match Arguments::parse().command {
        Commands::Root(command) => command.run(),
        Commands::Herdr(args) => args.command.run(),
    }
}
