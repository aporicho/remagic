use super::*;
use remagic_core::DeviceProduct;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[test]
fn packaged_runtime_files_must_not_be_symlinks_or_group_writable() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "remagic-agentd-runtime-security-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let target = root.join("node");
    fs::write(&target, b"runtime").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(secure_packaged_file(&target, true));
    fs::set_permissions(&target, fs::Permissions::from_mode(0o720)).unwrap();
    assert!(!secure_packaged_file(&target, true));
    let link = root.join("node-link");
    symlink(&target, &link).unwrap();
    assert!(!secure_packaged_file(&link, true));
    let _ = fs::remove_dir_all(root);
}

fn fixture() -> (AgentState, AppId, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "remagic-agentd-pi-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::write(&path, b"#!/bin/sh\nwhile IFS= read -r line; do :; done\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    (
        AgentState::new(
            DeviceProfile::for_product(DeviceProduct::PaperPro, "test"),
            path.clone(),
        ),
        AppId::new("magicpaper").unwrap(),
        path,
    )
}

fn profile() -> AgentProfile {
    AgentProfile {
        provider: "deepseek".into(),
        model: "deepseek-v4-flash".into(),
        thinking: "off".into(),
        tools: false,
    }
}

#[tokio::test]
async fn status_exposes_runtime_source_and_provider_configuration() {
    let (state, app, path) = fixture();
    let status = state.status(&app).await;
    assert!(status.available);
    assert_eq!(status.runtime_source, AgentRuntimeSource::Override);
    assert!(!status.provider_configured);
    state.reload_profile(&app, Some(profile())).await.unwrap();
    assert!(!state.status(&app).await.provider_configured);
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn new_session_is_idempotent_before_the_first_worker_exists() {
    let (state, app, path) = fixture();
    state.new_session(&app).await.unwrap();
    state.reload_profile(&app, Some(profile())).await.unwrap();
    state.new_session(&app).await.unwrap();
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn checking_an_unchanged_profile_preserves_the_warm_worker() {
    let (state, app, path) = fixture();
    let turn = state
        .start(
            &app,
            "r1",
            profile(),
            "system",
            AgentLane::Interactive,
            "foreground",
        )
        .await
        .unwrap();
    state.finish(&app, &turn.id).await;
    let before = state
        .inner
        .lock()
        .await
        .get(&app)
        .unwrap()
        .worker
        .as_ref()
        .unwrap()
        .clone();
    state.reload_profile(&app, Some(profile())).await.unwrap();
    let after = state
        .inner
        .lock()
        .await
        .get(&app)
        .unwrap()
        .worker
        .as_ref()
        .unwrap()
        .clone();
    assert!(before.same_worker(&after));
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn each_application_has_an_independent_turn_slot() {
    let (state, first, path) = fixture();
    let second = AppId::new("other-app").unwrap();
    let turn = state
        .start(
            &first,
            "r1",
            profile(),
            "system",
            AgentLane::Interactive,
            "foreground",
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .start(
                &first,
                "r2",
                profile(),
                "system",
                AgentLane::Interactive,
                "foreground",
            )
            .await
            .unwrap_err(),
        AgentErrorCode::Busy
    );
    assert!(state
        .start(
            &second,
            "r3",
            profile(),
            "system",
            AgentLane::Interactive,
            "foreground",
        )
        .await
        .is_ok());
    assert!(state.cancel(&first, &turn.id).await);
    state.finish(&first, &turn.id).await;
    assert!(!state.status(&first).await.busy);
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn concurrent_starts_cannot_both_claim_one_application_slot() {
    let (state, app, path) = fixture();
    let gate = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let mut attempts = Vec::new();
    for request in ["r1", "r2"] {
        let state = state.clone();
        let app = app.clone();
        let gate = gate.clone();
        attempts.push(tokio::spawn(async move {
            gate.wait().await;
            state
                .start(
                    &app,
                    request,
                    profile(),
                    "system",
                    AgentLane::Scheduled,
                    "background",
                )
                .await
        }));
    }
    gate.wait().await;
    let first = attempts.remove(0).await.unwrap();
    let second = attempts.remove(0).await.unwrap();
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let winner = first.ok().or_else(|| second.ok()).unwrap();
    state.finish(&app, &winner.id).await;
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn token_is_bound_to_cgroup_identity_and_generation() {
    let (state, app, path) = fixture();
    let first = ClientIdentity {
        app_id: app.clone(),
        generation: 7,
        principal: "foreground".into(),
    };
    let token = "a".repeat(64);
    assert!(state.authorize(&first, &app, &token).await);
    assert!(!state.authorize(&first, &app, &"b".repeat(64)).await);
    let other = AppId::new("other-app").unwrap();
    assert!(!state.authorize(&first, &other, &token).await);
    let stale = ClientIdentity {
        app_id: app.clone(),
        generation: 6,
        principal: "foreground".into(),
    };
    assert!(!state.authorize(&stale, &app, &token).await);
    let next = ClientIdentity {
        app_id: app.clone(),
        generation: 8,
        principal: "foreground".into(),
    };
    assert!(state.authorize(&next, &app, &"c".repeat(64)).await);
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn foreground_and_background_principals_authenticate_independently() {
    let (state, app, path) = fixture();
    let foreground = ClientIdentity {
        app_id: app.clone(),
        generation: 11,
        principal: "foreground".into(),
    };
    let background = ClientIdentity {
        app_id: app.clone(),
        generation: 12,
        principal: "background".into(),
    };
    let foreground_token = "a".repeat(64);
    let background_token = "b".repeat(64);
    assert!(state.authorize(&foreground, &app, &foreground_token).await);
    assert!(state.authorize(&background, &app, &background_token).await);
    assert!(!state.authorize(&foreground, &app, &background_token).await);
    assert!(!state.authorize(&background, &app, &foreground_token).await);
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn interactive_preempts_scheduled_but_scheduled_cannot_preempt_interactive() {
    let (state, app, path) = fixture();
    let mut scheduled = state
        .start(
            &app,
            "scheduled",
            profile(),
            "system",
            AgentLane::Scheduled,
            "background",
        )
        .await
        .unwrap();
    let waiting = state.clone();
    let waiting_app = app.clone();
    let next = tokio::spawn(async move {
        waiting
            .start(
                &waiting_app,
                "interactive",
                profile(),
                "system",
                AgentLane::Interactive,
                "foreground",
            )
            .await
    });
    scheduled.cancel.changed().await.unwrap();
    state.finish(&app, &scheduled.id).await;
    let interactive = next.await.unwrap().unwrap();
    assert_eq!(
        state
            .start(
                &app,
                "late-scheduled",
                profile(),
                "system",
                AgentLane::Scheduled,
                "background",
            )
            .await
            .unwrap_err(),
        AgentErrorCode::Busy
    );
    state.finish(&app, &interactive.id).await;
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn interactive_also_preempts_speculative_work() {
    let (state, app, path) = fixture();
    let mut speculative = state
        .start(
            &app,
            "speculative",
            profile(),
            "system",
            AgentLane::Speculative,
            "background",
        )
        .await
        .unwrap();
    let next_state = state.clone();
    let next_app = app.clone();
    let next = tokio::spawn(async move {
        next_state
            .start(
                &next_app,
                "interactive",
                profile(),
                "system",
                AgentLane::Interactive,
                "foreground",
            )
            .await
    });
    speculative.cancel.changed().await.unwrap();
    state.finish(&app, &speculative.id).await;
    let interactive = next.await.unwrap().unwrap();
    state.finish(&app, &interactive.id).await;
    fs::remove_file(path).unwrap();
}
