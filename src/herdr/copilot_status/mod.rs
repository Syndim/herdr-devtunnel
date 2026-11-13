use std::io;

use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use serde_json::Value;

#[cfg(test)]
mod tests;

#[derive(Args, Clone, Copy)]
pub struct Arguments {
    #[arg(value_enum)]
    event: Event,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Event {
    #[value(name = "sessionStart")]
    SessionStart,
    #[value(name = "userPromptSubmitted")]
    UserPromptSubmitted,
    #[value(name = "preToolUse")]
    PreToolUse,
    #[value(name = "postToolUse")]
    PostToolUse,
    #[value(name = "postToolUseFailure")]
    PostToolUseFailure,
    #[value(name = "permissionRequest")]
    PermissionRequest,
    #[value(name = "agentStop")]
    AgentStop,
    #[value(name = "sessionIdle")]
    SessionIdle,
    #[value(name = "errorOccurred")]
    ErrorOccurred,
    #[value(name = "sessionEnd")]
    SessionEnd,
}

impl Arguments {
    pub fn run(self) -> Result<()> {
        if std::env::var_os("HC_TERMINAL_ID").is_none_or(|value| value.is_empty()) {
            return Ok(());
        }
        let payload = serde_json::from_reader(io::stdin().lock())
            .context("Read Copilot hook JSON from stdin")?;
        if let Some(sequence) = sequence(self.event, &payload)? {
            write_terminal(sequence.as_bytes())?;
        }
        Ok(())
    }
}

fn sequence(event: Event, payload: &Value) -> Result<Option<String>> {
    ensure!(
        payload.is_object(),
        "Copilot hook input must be a JSON object"
    );
    if ["agentId", "agent_id"].iter().any(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
    }) {
        return Ok(None);
    }
    let tool = payload
        .get("toolName")
        .or_else(|| payload.get("tool_name"))
        .and_then(Value::as_str);
    let state = match event {
        Event::SessionStart => "idle",
        Event::PreToolUse if matches!(tool, Some("ask_user" | "AskUserQuestion")) => "blocked",
        Event::UserPromptSubmitted
        | Event::PreToolUse
        | Event::PostToolUse
        | Event::PostToolUseFailure => "working",
        // This fires before auto-approval, not when a user prompt is displayed.
        Event::PermissionRequest => return Ok(None),
        // A foreground turn can stop while attached background work is running.
        Event::AgentStop => return Ok(None),
        Event::SessionIdle if payload.get("aborted") == Some(&Value::Bool(true)) => "idle",
        Event::SessionIdle => "done",
        Event::ErrorOccurred => "error",
        Event::SessionEnd if payload.get("reason").and_then(Value::as_str) == Some("user_exit") => {
            "clear"
        }
        // "complete" also fires between foreground turns with attached work.
        Event::SessionEnd => return Ok(None),
    };
    Ok(Some(format!("\x1b]7501;app=copilot:state={state}\x1b\\")))
}

fn write_terminal(sequence: &[u8]) -> Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole};

        // Copilot hooks get a private console; stdout is captured by the CLI.
        unsafe {
            FreeConsole().context("Detach Copilot hook's private console")?;
            AttachConsole(ATTACH_PARENT_PROCESS).context("Attach Copilot's terminal console")?;
        }
    }
    super::terminal_status::write_terminal(sequence)
        .context("Write Copilot status to Herdr terminal")
}
