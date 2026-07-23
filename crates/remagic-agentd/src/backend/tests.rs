use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[test]
fn assistant_content_excludes_reasoning() {
    let event = json!({"message": {"role":"assistant", "content":[
        {"type":"thinking", "thinking":"secret"},
        {"type":"text", "text":"paper answer"}
    ]}});
    assert_eq!(assistant_text(&event).as_deref(), Some("paper answer"));
}

#[test]
fn official_rpc_streaming_delta_is_available_as_a_fallback() {
    let event = json!({
        "type": "message_update",
        "message": {"role":"assistant", "content":[{"type":"text", "text":"Hello"}]},
        "assistantMessageEvent": {"type":"text_delta", "contentIndex":0, "delta":"lo"}
    });
    assert_eq!(streaming_text_delta(&event), Some("lo"));
}

#[test]
fn response_requires_matching_id_command_and_success_is_separate() {
    let response = json!({"id":"t1","type":"response","command":"prompt","success":true});
    assert!(response_matches(&response, "t1", "prompt"));
    assert!(!response_matches(&response, "t2", "prompt"));
}

#[tokio::test]
async fn one_worker_handles_multiple_rpc_turns_and_new_session() {
    let path = std::env::temp_dir().join(format!(
        "remagic-agentd-rpc-fixture-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::write(
        &path,
        br##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *new_session*)
      echo '{"id":"new-session:2","type":"response","command":"new_session","success":true,"data":{"cancelled":false}}'
      ;;
    *magicpaper:r1*)
      echo '{"id":"magicpaper:r1","type":"response","command":"prompt","success":true}'
      echo '{"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"first"}]}}'
      echo '{"type":"agent_end"}'
      ;;
    *magicpaper:r2*)
      echo '{"id":"magicpaper:r2","type":"response","command":"prompt","success":true}'
      echo '{"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"second"}]}}'
      echo '{"type":"agent_end"}'
      ;;
    *old-user*)
      echo '{"id":"magicpaper:r3","type":"response","command":"prompt","success":false,"error":"old history was rehydrated"}'
      ;;
    *magicpaper:r3*)
      echo '{"id":"magicpaper:r3","type":"response","command":"prompt","success":true}'
      echo '{"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"clean"}]}}'
      echo '{"type":"agent_end"}'
      ;;
  esac
done
"##,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let app = AppId::new("magicpaper").unwrap();
    let profile = AgentProfile {
        provider: "deepseek".into(),
        model: "deepseek-v4-flash".into(),
        thinking: "off".into(),
        tools: false,
    };
    let worker = WorkerHandle::spawn(&path, &app, &profile, "system")
        .await
        .unwrap();
    for (request_id, expected) in [("r1", "first"), ("r2", "second")] {
        let (events, mut received) = mpsc::channel(8);
        let (_cancel, cancel) = watch::channel(false);
        worker
            .turn(
                TurnInput {
                    request_id: request_id.into(),
                    app_id: app.clone(),
                    turn_id: format!("magicpaper:{request_id}"),
                    input: "hello".into(),
                    history: Vec::new(),
                },
                cancel,
                events,
            )
            .await
            .unwrap();
        let delta = received.recv().await.unwrap();
        assert!(matches!(delta, AgentEvent::TextDelta { text, .. } if text == expected));
        assert!(matches!(
            received.recv().await,
            Some(AgentEvent::Complete { .. })
        ));
    }
    worker.new_session().await.unwrap();
    let (events, mut received) = mpsc::channel(8);
    let (_cancel, cancel) = watch::channel(false);
    worker
        .turn(
            TurnInput {
                request_id: "r3".into(),
                app_id: app.clone(),
                turn_id: "magicpaper:r3".into(),
                input: "fresh".into(),
                history: vec![AgentHistoryTurn {
                    user: "old-user".into(),
                    assistant: "old-assistant".into(),
                }],
            },
            cancel,
            events,
        )
        .await
        .unwrap();
    assert!(matches!(
        received.recv().await,
        Some(AgentEvent::TextDelta { text, .. }) if text == "clean"
    ));
    worker.shutdown().await;
    fs::remove_file(path).unwrap();
}

#[test]
fn local_history_is_hydrated_only_when_requested() {
    let turn = TurnInput {
        request_id: "r1".into(),
        app_id: AppId::new("magicpaper").unwrap(),
        turn_id: "magicpaper:r1".into(),
        input: "current".into(),
        history: vec![AgentHistoryTurn {
            user: "old-user".into(),
            assistant: "old-assistant".into(),
        }],
    };
    assert!(compose_prompt(&turn, true).contains("old-user"));
    assert_eq!(compose_prompt(&turn, false), "User: current");
}

#[test]
fn cumulative_published_output_is_bounded() {
    let mut published = "a".repeat(MAX_PUBLISHED_OUTPUT);
    let event = json!({
        "assistantMessageEvent": {"type":"text_delta", "delta":"b"}
    });
    assert!(update_candidate(&event, &mut published).is_err());
    assert_eq!(published.len(), MAX_PUBLISHED_OUTPUT);
}

#[test]
fn a_new_full_assistant_message_replaces_tool_narration() {
    let mut published = "first answer".to_owned();
    let event = json!({
        "message": {"role":"assistant", "content":[{"type":"text", "text":"rewritten"}]}
    });
    update_candidate(&event, &mut published).unwrap();
    assert_eq!(published, "rewritten");
}

#[tokio::test]
async fn only_the_post_tool_final_answer_is_published_to_the_application() {
    let turn = TurnInput {
        request_id: "r1".into(),
        app_id: AppId::new("magicpaper").unwrap(),
        turn_id: "magicpaper:r1".into(),
        input: "current".into(),
        history: Vec::new(),
    };
    let (events, mut received) = mpsc::channel(4);
    let mut candidate = "I will search now".to_owned();
    let mut accepted = true;
    assert!(matches!(
        process_line(
            r#"{"type":"tool_execution_start"}"#,
            &turn,
            &events,
            &mut candidate,
            &mut accepted,
        )
        .await,
        LineOutcome::Continue
    ));
    assert!(candidate.is_empty());
    process_line(
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"最终答案"}]}}"#,
        &turn,
        &events,
        &mut candidate,
        &mut accepted,
    )
    .await;
    assert!(matches!(
        process_line(
            r#"{"type":"agent_end"}"#,
            &turn,
            &events,
            &mut candidate,
            &mut accepted,
        )
        .await,
        LineOutcome::Complete
    ));
    assert!(matches!(
        received.recv().await,
        Some(AgentEvent::TextDelta { text, .. }) if text == "最终答案"
    ));
    assert!(matches!(
        received.recv().await,
        Some(AgentEvent::Complete { .. })
    ));
}

#[tokio::test]
async fn cancellation_sends_abort_and_waits_for_response_and_agent_end() {
    let path = std::env::temp_dir().join(format!(
        "remagic-agentd-abort-fixture-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::write(
        &path,
        br##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *abort:magicpaper:r1*)
      echo '{"id":"abort:magicpaper:r1","type":"response","command":"abort","success":true}'
      echo '{"type":"agent_end"}'
      ;;
    *magicpaper:r1*)
      echo '{"id":"magicpaper:r1","type":"response","command":"prompt","success":true}'
      echo '{"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"started"}]},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"started"}}'
      ;;
  esac
done
"##,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let app = AppId::new("magicpaper").unwrap();
    let profile = AgentProfile {
        provider: "deepseek".into(),
        model: "deepseek-v4-flash".into(),
        thinking: "off".into(),
        tools: false,
    };
    let worker = WorkerHandle::spawn(&path, &app, &profile, "system")
        .await
        .unwrap();
    let (events, mut received) = mpsc::channel(8);
    let (cancel, receiver) = watch::channel(false);
    let active = {
        let worker = worker.clone();
        let app = app.clone();
        tokio::spawn(async move {
            worker
                .turn(
                    TurnInput {
                        request_id: "r1".into(),
                        app_id: app,
                        turn_id: "magicpaper:r1".into(),
                        input: "hello".into(),
                        history: Vec::new(),
                    },
                    receiver,
                    events,
                )
                .await
        })
    };
    // Replies are intentionally buffered until `agent_end`, so cancellation
    // must not wait for a visible text event before it can interrupt Pi.
    cancel.send(true).unwrap();
    assert!(active.await.unwrap().is_ok());
    assert!(matches!(
        received.recv().await,
        Some(AgentEvent::Error {
            code: AgentErrorCode::Cancelled,
            ..
        })
    ));
    worker.shutdown().await;
    fs::remove_file(path).unwrap();
}
