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
fn foreground_stop_does_not_report_background_work_as_complete() {
    assert!(
        sequence(Event::AgentStop, &json!({"stopReason": "end_turn"}))
            .unwrap()
            .is_none()
    );
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
