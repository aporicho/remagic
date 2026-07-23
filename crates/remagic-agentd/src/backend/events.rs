//! Pi RPC event reduction into the bounded application-facing stream.

use super::TurnInput;
use remagic_protocol::{AgentErrorCode, AgentEvent, AGENT_PROTOCOL_V1};
use serde_json::Value;
use tokio::sync::mpsc;

pub(super) const MAX_PUBLISHED_OUTPUT: usize = 64 * 1024;

pub(super) enum LineOutcome {
    Continue,
    Complete,
    Rejected(String),
    Fatal(String),
}

pub(super) async fn process_line(
    line: &str,
    turn: &TurnInput,
    events: &mpsc::Sender<AgentEvent>,
    published: &mut String,
    accepted: &mut bool,
) -> LineOutcome {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return LineOutcome::Continue;
    };
    if response_matches(&value, &turn.turn_id, "prompt") {
        *accepted = value.get("success").and_then(Value::as_bool) == Some(true);
        return if *accepted {
            LineOutcome::Continue
        } else {
            LineOutcome::Rejected(rpc_error(&value))
        };
    }
    match value.get("type").and_then(Value::as_str) {
        Some("message_update") | Some("message_end") if *accepted => {
            match update_candidate(&value, published) {
                Ok(()) => LineOutcome::Continue,
                Err(message) => LineOutcome::Fatal(message.into()),
            }
        }
        Some("tool_execution_start") if *accepted => {
            // Any assistant prose preceding a tool is working narration, not
            // the final paper answer. The post-tool assistant message replaces it.
            published.clear();
            LineOutcome::Continue
        }
        Some("agent_end") if *accepted => {
            if !published.is_empty() {
                publish_text(events, turn, published).await;
            }
            let _ = events
                .send(AgentEvent::Complete {
                    protocol: AGENT_PROTOCOL_V1,
                    request_id: turn.request_id.clone(),
                    app_id: turn.app_id.clone(),
                    turn_id: turn.turn_id.clone(),
                })
                .await;
            LineOutcome::Complete
        }
        _ => LineOutcome::Continue,
    }
}

pub(super) fn update_candidate(value: &Value, candidate: &mut String) -> Result<(), &'static str> {
    if let Some(text) = assistant_text(value) {
        if text.len() > MAX_PUBLISHED_OUTPUT {
            return Err("Pi answer exceeded the 64 KiB output limit");
        }
        *candidate = text;
        return Ok(());
    }
    if let Some(delta) = streaming_text_delta(value).filter(|delta| !delta.is_empty()) {
        if candidate.len().saturating_add(delta.len()) > MAX_PUBLISHED_OUTPUT {
            return Err("Pi answer exceeded the 64 KiB output limit");
        }
        candidate.push_str(delta);
    }
    Ok(())
}

pub(super) fn streaming_text_delta(value: &Value) -> Option<&str> {
    let event = value.get("assistantMessageEvent")?;
    (event.get("type").and_then(Value::as_str) == Some("text_delta"))
        .then(|| event.get("delta").and_then(Value::as_str))
        .flatten()
}

async fn publish_text(events: &mpsc::Sender<AgentEvent>, turn: &TurnInput, text: &str) {
    let _ = events
        .send(AgentEvent::TextDelta {
            protocol: AGENT_PROTOCOL_V1,
            request_id: turn.request_id.clone(),
            app_id: turn.app_id.clone(),
            turn_id: turn.turn_id.clone(),
            text: text.into(),
        })
        .await;
}

pub(super) fn response_matches(value: &Value, id: &str, command: &str) -> bool {
    value.get("type").and_then(Value::as_str) == Some("response")
        && value.get("id").and_then(Value::as_str) == Some(id)
        && value.get("command").and_then(Value::as_str) == Some(command)
}

pub(super) fn rpc_error(value: &Value) -> String {
    value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("Pi rejected the RPC command")
        .into()
}

pub(super) fn assistant_text(value: &Value) -> Option<String> {
    let message = value.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let content = message.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

pub(super) async fn send_error(
    events: &mpsc::Sender<AgentEvent>,
    turn: &TurnInput,
    message: impl Into<String>,
    retryable: bool,
) {
    let _ = events
        .send(AgentEvent::Error {
            protocol: AGENT_PROTOCOL_V1,
            request_id: turn.request_id.clone(),
            app_id: turn.app_id.clone(),
            turn_id: Some(turn.turn_id.clone()),
            code: AgentErrorCode::BackendFailed,
            message: message.into(),
            retryable,
        })
        .await;
}

pub(super) async fn send_cancelled(events: &mpsc::Sender<AgentEvent>, turn: &TurnInput) {
    let _ = events
        .send(AgentEvent::Error {
            protocol: AGENT_PROTOCOL_V1,
            request_id: turn.request_id.clone(),
            app_id: turn.app_id.clone(),
            turn_id: Some(turn.turn_id.clone()),
            code: AgentErrorCode::Cancelled,
            message: "turn cancelled".into(),
            retryable: false,
        })
        .await;
}
