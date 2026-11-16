mod caller;
pub(crate) mod copilot_status;
mod integrate;
mod terminal_status;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

#[derive(Args)]
pub struct Arguments {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install user-level lifecycle hooks for an agent provider.
    Integrate(integrate::Arguments),
    /// Report a lifecycle event, detecting the provider from the calling process.
    Status(StatusArguments),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum Provider {
    Copilot,
}

#[derive(Args)]
struct StatusArguments {
    #[command(flatten)]
    hook: copilot_status::Arguments,
    /// Override caller detection (for wrappers or manual invocation).
    #[arg(long, value_enum)]
    provider: Option<Provider>,
}

impl Arguments {
    pub fn run(self) -> Result<()> {
        match self.command {
            Commands::Integrate(args) => args.run(),
            Commands::Status(args) => {
                // Hooks outside Herdr must not inspect processes or wait on stdin.
                if std::env::var_os("HC_TERMINAL_ID").is_none_or(|value| value.is_empty()) {
                    return Ok(());
                }
                match args.provider.or_else(caller::detect) {
                    Some(Provider::Copilot) => args.hook.run(),
                    // Unknown callers must not be mislabeled as Copilot.
                    None => Ok(()),
                }
            }
        }
    }
}
