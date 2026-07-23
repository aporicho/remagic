use super::*;
use remagic_core::DeviceProduct;
use remagic_protocol::{AgentLane, AgentProfile};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

fn state_fixture() -> (AgentState, AppId, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "remagic-agentd-disconnect-{}-{}",
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
async fn disconnect_cancels_only_an_owned_active_turn() {
    let (state, app, path) = state_fixture();
    let mut started = state
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
    let owned = ConnectionTurn::new(Some((app.clone(), started.id.clone())));
    assert!(cancel_connection_turn(&state, &owned).await);
    started.cancel.changed().await.unwrap();
    state.finish(&app, &started.id).await;
    assert!(!cancel_connection_turn(&state, &owned).await);
    fs::remove_file(path).unwrap();
}

#[test]
fn managed_environment_fields_are_nul_delimited() {
    let environment = b"A=x\0REMAGIC_APP_ID=magicpaper\0REMAGIC_APP_GENERATION=17\0";
    assert_eq!(
        process_environment(environment, "REMAGIC_APP_ID").as_deref(),
        Some("magicpaper")
    );
    assert!(process_environment(environment, "MISSING").is_none());
}

#[test]
fn foreground_and_background_units_are_distinct() {
    let app = AppId::new("magicpaper").unwrap();
    assert_eq!(
        principal_unit(&app, "foreground").as_deref(),
        Some("remagic-app@magicpaper.service")
    );
    assert_eq!(
        principal_unit(&app, "background").as_deref(),
        Some("remagic-background-magicpaper.service")
    );
    assert!(principal_unit(&app, "unknown").is_none());
}

#[test]
fn cgroup_unit_match_requires_an_exact_path_component() {
    let cgroup = "0::/system.slice/remagic-app@magicpaper.service\n";
    assert!(cgroup_contains_unit(
        cgroup,
        "remagic-app@magicpaper.service"
    ));
    assert!(!cgroup_contains_unit(cgroup, "remagic-app@magic.service"));
    assert!(!cgroup_contains_unit(
        "0::/system.slice/remagic-app@magicpaper.service-shadow\n",
        "remagic-app@magicpaper.service"
    ));
}
