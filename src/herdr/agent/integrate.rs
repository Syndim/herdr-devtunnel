use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};

use super::Provider;

#[derive(Args)]
pub(super) struct Arguments {
    #[arg(long, value_enum)]
    provider: Provider,
}

impl Arguments {
    pub(super) fn run(self) -> Result<()> {
        let executable = std::env::current_exe().context("Locate hd executable")?;
        let path = match self.provider {
            Provider::Copilot => copilot_home()?.join("hooks/herdr-devtunnel.json"),
        };
        let config = match self.provider {
            Provider::Copilot => copilot_hooks(&executable)?,
        };
        install(&path, &config)?;
        println!("User hooks installed: {}", path.display());
        println!("Restart Copilot to load the hooks. Keep the hd executable at its current path.");
        Ok(())
    }
}

fn copilot_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("COPILOT_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".copilot"))
        .with_context(|| format!("Set {variable} or COPILOT_HOME to install user hooks"))
}

fn copilot_hooks(executable: &Path) -> Result<Value> {
    let executable = executable
        .to_str()
        .context("The hd executable path must be valid UTF-8 for Copilot hook JSON")?;
    let mut hooks = serde_json::Map::new();
    // sessionIdle is an extension event, not a configurable CLI hook.
    for event in [
        "sessionStart",
        "userPromptSubmitted",
        "preToolUse",
        "postToolUse",
        "postToolUseFailure",
        "permissionRequest",
        "agentStop",
        "errorOccurred",
        "sessionEnd",
    ] {
        hooks.insert(
            event.into(),
            json!([{
                "type": "command",
                "exec": executable,
                "args": ["agent", "status", event],
                "timeoutSec": 5
            }]),
        );
    }
    Ok(json!({"version": 1, "hooks": hooks}))
}

fn install(path: &Path, config: &Value) -> Result<()> {
    let mut contents = serde_json::to_vec_pretty(config)?;
    contents.push(b'\n');
    fs::create_dir_all(path.parent().context("Hook file has no parent directory")?)
        .context("Create user hooks directory")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            if let Err(error) = file.write_all(&contents).and_then(|_| file.sync_all()) {
                drop(file);
                let _ = fs::remove_file(path);
                return Err(error).context("Write user hooks");
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Never replace another integration or follow a pre-existing symlink.
            let metadata = fs::symlink_metadata(path).context("Inspect existing hook file")?;
            if metadata.is_file() && fs::read(path).context("Read existing hook file")? == contents
            {
                return Ok(());
            }
            bail!(
                "Refusing to overwrite existing hook file: {}. Review and remove it before reinstalling",
                path.display()
            )
        }
        Err(error) => Err(error).with_context(|| format!("Create hook file: {}", path.display())),
    }
}
