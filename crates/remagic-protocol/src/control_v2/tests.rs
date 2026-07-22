use super::*;
use remagic_core::{
    AppInstanceState, AppToken, ManagerState, PreflightCheck, PreflightStatus, SessionStatus,
    SystemDomainState,
};
use serde::de::DeserializeOwned;
use std::collections::BTreeSet;
use std::fmt::Debug;

fn id(value: &str) -> AppId {
    AppId::new(value).unwrap()
}

fn snapshot() -> SupervisorSnapshot {
    let app_id = id("koreader");
    let instance = AppInstance {
        token: AppToken {
            app_id: app_id.clone(),
            generation: 9,
            foreground_epoch: 3,
            lease_id: Some(77),
        },
        state: AppInstanceState::Foreground,
        pid: Some(123),
        title: "KOReader".into(),
        subtitle: "Reading".into(),
        last_error: None,
    };
    let state = SupervisorState {
        domain: SystemDomainState::Managed,
        sleeping: false,
        foreground_app: Some(app_id.clone()),
        last_app: Some(app_id.clone()),
        apps: [(app_id.clone(), instance.clone())].into(),
        state_revision: 51,
    };
    SupervisorSnapshot {
        state,
        apps: vec![AppViewV2 {
            id: app_id.clone(),
            name: "KOReader".into(),
            description: "Reader".into(),
            version: "2026.03".into(),
            kind: remagic_core::AppKind::User,
            installed: true,
            runtime_profile: RuntimeProfile::QtfbCompat,
            capabilities: vec![Capability::new("display:qtfb-v1").unwrap()],
            instance: Some(instance),
            background_service: None,
            background_active: false,
            session: Some(AppSession {
                schema: 1,
                app_id,
                status: SessionStatus::Background,
                title: "KOReader".into(),
                subtitle: String::new(),
                resume_payload: None,
                updated_at: 1,
                last_error: None,
            }),
            package: Some("koreader".into()),
            supported_devices: vec![remagic_core::DeviceProduct::PaperProMove],
            supported_os: Vec::new(),
            required_remagic_api: 2,
            uninstall_policy: remagic_core::UninstallPolicy::KeepData,
            preflight: None,
        }],
    }
}

fn round_trip<T>(value: T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = serde_json::to_vec(&value).unwrap();
    let decoded: T = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, value);
}

fn failed_preflight() -> PreflightReport {
    PreflightReport {
        app_id: id("magicpaper"),
        profile: RuntimeProfile::NativeV2,
        compatible: false,
        checks: vec![PreflightCheck {
            id: "display-owner".into(),
            status: PreflightStatus::Failed,
            message: "busy".into(),
        }],
        missing_capabilities: BTreeSet::new(),
        missing_libraries: Vec::new(),
        launch_environment: None,
    }
}

#[test]
fn every_control_message_family_round_trips() {
    let values = [
        serde_json::to_value(Envelope::new(
            "request-1",
            ControlIntent::Launch {
                app_id: id("magicpaper"),
                open_path: None,
            },
        ))
        .unwrap(),
        serde_json::to_value(Envelope::new(
            "response-1",
            ControlReply::Snapshot {
                snapshot: snapshot(),
            },
        ))
        .unwrap(),
        serde_json::to_value(Envelope::new(
            "event-1",
            ControlEvent::AppChanged {
                app_id: id("magicpaper"),
                instance: None,
                state_revision: 52,
            },
        ))
        .unwrap(),
    ];
    let request: ControlRequest = serde_json::from_value(values[0].clone()).unwrap();
    let response: ControlResponse = serde_json::from_value(values[1].clone()).unwrap();
    let event: ControlEventEnvelope = serde_json::from_value(values[2].clone()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), values[0]);
    assert_eq!(serde_json::to_value(response).unwrap(), values[1]);
    assert_eq!(serde_json::to_value(event).unwrap(), values[2]);
}

#[test]
fn every_control_intent_variant_round_trips() {
    let app = id("magicpaper");
    let intents = vec![
        ControlIntent::Snapshot,
        ControlIntent::Subscribe {
            since_revision: Some(10),
        },
        ControlIntent::ReloadManifests,
        ControlIntent::ShowHome,
        ControlIntent::ReturnStock,
        ControlIntent::Sleep,
        ControlIntent::Wake,
        ControlIntent::Launch {
            app_id: app.clone(),
            open_path: Some("/home/root/books/test.epub".into()),
        },
        ControlIntent::OpenPath {
            app_id: app.clone(),
            path: "/home/root/books/test.epub".into(),
        },
        ControlIntent::ParkCurrent,
        ControlIntent::Close {
            app_id: app.clone(),
        },
        ControlIntent::Preflight {
            app_id: app.clone(),
        },
        ControlIntent::Install {
            bundle: "/tmp/app.bundle".into(),
        },
        ControlIntent::Upgrade {
            app_id: app.clone(),
            bundle: None,
        },
        ControlIntent::Rollback {
            app_id: app.clone(),
            version: Some("0.1.0".into()),
        },
        ControlIntent::Uninstall {
            app_id: app,
            purge: true,
        },
        ControlIntent::LegacyPackage {
            operation: PackageOperation::Search {
                query: "reader".into(),
            },
        },
    ];
    for intent in intents {
        round_trip(intent);
    }
}

#[test]
fn every_control_reply_and_event_variant_round_trips() {
    let app = id("magicpaper");
    for reply in [
        ControlReply::Ack { state_revision: 1 },
        ControlReply::Snapshot {
            snapshot: snapshot(),
        },
        ControlReply::Subscribed { state_revision: 2 },
        ControlReply::Preflight {
            report: Box::new(failed_preflight()),
        },
        ControlReply::PackageOutput {
            success: true,
            output: "done".into(),
            state_revision: 3,
        },
        ControlReply::Error {
            code: ControlErrorCode::RevisionConflict,
            message: "stale".into(),
            state_revision: Some(4),
        },
    ] {
        round_trip(reply);
    }

    for event in [
        ControlEvent::SnapshotChanged {
            snapshot: snapshot(),
        },
        ControlEvent::DomainChanged {
            domain: SystemDomainState::Managed,
            sleeping: false,
            state_revision: 5,
        },
        ControlEvent::AppChanged {
            app_id: app.clone(),
            instance: None,
            state_revision: 6,
        },
        ControlEvent::Notification {
            app_id: app.clone(),
            title: "Title".into(),
            body: "Body".into(),
            state_revision: 7,
        },
        ControlEvent::PackageProgress {
            app_id: app,
            phase: "install".into(),
            completed: 1,
            total: 2,
            state_revision: 8,
        },
    ] {
        round_trip(event);
    }
}

#[test]
fn snapshot_has_lossy_but_stable_v1_views() {
    let snapshot = snapshot();
    assert_eq!(
        snapshot.to_v1_status(),
        crate::Response::Status {
            domain: remagic_core::DomainState::Foreground(id("koreader")),
            last_app: Some(id("koreader")),
            sequence: 51,
        }
    );
    let crate::Response::Apps { apps } = snapshot.to_v1_apps() else {
        panic!("wrong response")
    };
    assert_eq!(apps.len(), 1);
    assert!(apps[0].foreground);
    assert_eq!(ManagerState::from(&snapshot.state).sequence, 51);
}

#[test]
fn legacy_app_callbacks_cannot_enter_control_v2() {
    let result = ControlIntent::try_from(crate::Request::Ready {
        app_id: id("magicpaper"),
    });
    assert_eq!(result, Err(LegacyControlConversionError::LifecycleRequest));
}
