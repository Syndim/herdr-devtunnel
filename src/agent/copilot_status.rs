use std::io;

use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use serde_json::Value;

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
        // The CLI hook reports foreground completion. SessionIdle can report
        // when attached background work has also finished.
        Event::AgentStop => match payload
            .get("stopReason")
            .or_else(|| payload.get("stop_reason"))
            .and_then(Value::as_str)
        {
            Some("end_turn") => "done",
            _ => return Ok(None),
        },
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
    write_terminal_inner(sequence).context("Write Copilot status to Herdr terminal")
}

fn write_terminal_inner(sequence: &[u8]) -> Result<()> {
    use std::io::Write;
    let device = if cfg!(windows) {
        r"\\.\CONOUT$"
    } else {
        "/dev/tty"
    };
    std::fs::OpenOptions::new()
        .write(true)
        .open(device)
        .with_context(|| format!("Open Herdr terminal {device}"))?
        .write_all(sequence)
        .context("Write status to Herdr terminal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifecycle_states() {
        for (event, state) in [
            (Event::SessionStart, "idle"),
            (Event::UserPromptSubmitted, "working"),
            (Event::PreToolUse, "working"),
            (Event::PostToolUse, "working"),
            (Event::PostToolUseFailure, "working"),
            (Event::SessionIdle, "done"),
            (Event::ErrorOccurred, "error"),
        ] {
            assert_eq!(
                sequence(event, &json!({})).unwrap(),
                Some(format!("\x1b]7501;app=copilot:state={state}\x1b\\"))
            );
        }
        for key in ["toolName", "tool_name"] {
            for name in ["ask_user", "AskUserQuestion"] {
                assert_eq!(
                    sequence(Event::PreToolUse, &json!({key: name})).unwrap(),
                    Some("\x1b]7501;app=copilot:state=blocked\x1b\\".into())
                );
            }
        }
        assert_eq!(
            sequence(Event::SessionEnd, &json!({"reason": "user_exit"})).unwrap(),
            Some("\x1b]7501;app=copilot:state=clear\x1b\\".into())
        );
    }

    #[test]
    fn root_agent_stop_reports_foreground_completion() {
        for key in ["stopReason", "stop_reason"] {
            assert_eq!(
                sequence(Event::AgentStop, &json!({key: "end_turn"})).unwrap(),
                Some("\x1b]7501;app=copilot:state=done\x1b\\".into())
            );
        }
        for payload in [json!({}), json!({"stopReason": "unknown"})] {
            assert!(sequence(Event::AgentStop, &payload).unwrap().is_none());
        }
    }

    #[test]
    fn session_end_ignores_non_exit_reasons_and_aborted_idle_reports_idle() {
        for reason in ["complete", "abort", "timeout", "error"] {
            assert!(
                sequence(Event::SessionEnd, &json!({"reason": reason}))
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            sequence(Event::SessionIdle, &json!({"aborted": true})).unwrap(),
            Some("\x1b]7501;app=copilot:state=idle\x1b\\".into())
        );
    }

    #[test]
    fn permission_checks_do_not_report_attention_before_approval() {
        for payload in [
            json!({}),
            json!({"toolName": "bash", "toolInput": {"command": "cargo test"}}),
            json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"}}),
        ] {
            assert!(
                sequence(Event::PermissionRequest, &payload)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn child_events_do_not_overwrite_root_status() {
        for key in ["agentId", "agent_id"] {
            assert!(
                sequence(Event::SessionIdle, &json!({key: "child"}))
                    .unwrap()
                    .is_none()
            );
            assert!(
                sequence(Event::SessionIdle, &json!({key: ""}))
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    fn non_object_payload_is_rejected() {
        for payload in [json!([]), json!(null), json!("bad")] {
            assert!(sequence(Event::SessionStart, &payload).is_err());
        }
    }
}
