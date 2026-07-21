use super::*;
use remagic_core::{
    AppId, CertificatePolicy, FontPolicy, LocalePolicy, NetworkPolicy, RuntimeDirectories,
    RuntimeProfile, TimezonePolicy,
};
use std::collections::{BTreeMap, BTreeSet};

fn token() -> AppToken {
    AppToken {
        app_id: AppId::new("koreader").unwrap(),
        generation: 4,
        foreground_epoch: 2,
        lease_id: Some(88),
    }
}

fn environment() -> LaunchEnvironment {
    let directories = RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/koreader".into(),
        data_home: "/home/root/.local/share/koreader".into(),
        state_home: "/home/root/.local/state/koreader".into(),
        cache_home: "/home/root/.cache/koreader".into(),
        runtime_dir: "/run/user/0/remagic/koreader".into(),
    };
    LaunchEnvironment {
        app_id: token().app_id,
        profile: RuntimeProfile::QtfbCompat,
        variables: BTreeMap::from([
            ("HOME".into(), directories.home.display().to_string()),
            (
                "XDG_CONFIG_HOME".into(),
                directories.config_home.display().to_string(),
            ),
            (
                "XDG_DATA_HOME".into(),
                directories.data_home.display().to_string(),
            ),
            (
                "XDG_STATE_HOME".into(),
                directories.state_home.display().to_string(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                directories.cache_home.display().to_string(),
            ),
            (
                "XDG_RUNTIME_DIR".into(),
                directories.runtime_dir.display().to_string(),
            ),
            ("LANG".into(), "C.UTF-8".into()),
            ("TZ".into(), "UTC".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("REMAGIC_APP_ID".into(), "koreader".into()),
            ("REMAGIC_RUNTIME_PROFILE".into(), "qtfb_compat".into()),
            ("REMAGIC_NETWORK_POLICY_MODE".into(), "deny".into()),
            (
                "REMAGIC_NETWORK_POLICY_ENFORCEMENT".into(),
                "metadata_only".into(),
            ),
            ("REMAGIC_NETWORK_ISOLATED".into(), "0".into()),
            ("REMAGIC_NETWORK_ALLOWED_HOSTS".into(), "".into()),
        ]),
        directories,
        resolved_libraries: Vec::new(),
        platform_capabilities: BTreeSet::new(),
        locale: LocalePolicy::default(),
        timezone: TimezonePolicy::default(),
        fonts: FontPolicy::default(),
        certificates: CertificatePolicy::default(),
        network: NetworkPolicy::default(),
    }
}

#[test]
fn command_and_event_have_token_at_body_top_level() {
    let command = Envelope::new(
        "cmd-1",
        LifecycleCommandBody {
            token: token(),
            command: LifecycleCommand::Start {
                launch_environment: Box::new(environment()),
                resume_payload: None,
                open_path: None,
            },
        },
    );
    let value = serde_json::to_value(&command).unwrap();
    assert_eq!(value["body"]["command"], "start");
    assert_eq!(value["body"]["token"]["generation"], 4);
    let decoded: LifecycleCommandEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, command);

    let event = Envelope::new(
        "event-1",
        LifecycleEventBody {
            token: token(),
            event: LifecycleEvent::Ready {
                first_frame_sequence: Some(7),
            },
        },
    );
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["body"]["event"], "ready");
    let decoded: LifecycleEventEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn legacy_callback_requires_matching_app_and_generation() {
    let event = LifecycleEventBody::from_v1_request(
        crate::Request::RuntimeExited {
            app_id: token().app_id,
            generation: 4,
            exit_code: 0,
            crashed: false,
        },
        token(),
    )
    .unwrap();
    assert!(matches!(
        event.event,
        LifecycleEvent::ShutdownComplete { exit_code: 0 }
    ));

    let error = LifecycleEventBody::from_v1_request(
        crate::Request::RuntimeExited {
            app_id: token().app_id,
            generation: 3,
            exit_code: 0,
            crashed: false,
        },
        token(),
    );
    assert!(matches!(
        error,
        Err(LifecycleCompatibilityError::TokenGenerationMismatch {
            expected: 4,
            actual: 3
        })
    ));
}

#[test]
fn all_lifecycle_variants_round_trip() {
    let commands = vec![
        LifecycleCommand::Start {
            launch_environment: Box::new(environment()),
            resume_payload: None,
            open_path: None,
        },
        LifecycleCommand::EnterForeground {
            resume_payload: Some(serde_json::json!({"page": 9})),
            open_path: None,
        },
        LifecycleCommand::EnterBackground,
        LifecycleCommand::OpenPath {
            path: "/home/root/books/test.epub".into(),
        },
        LifecycleCommand::Shutdown {
            reason: ShutdownReason::ReturnStock,
            deadline_ms: 5_500,
        },
    ];
    for command in commands {
        let body = LifecycleCommandBody {
            token: token(),
            command,
        };
        assert!(body.validate().is_ok());
        let value = serde_json::to_vec(&body).unwrap();
        let decoded: LifecycleCommandBody = serde_json::from_slice(&value).unwrap();
        assert_eq!(decoded, body);
    }

    let events = vec![
        LifecycleEvent::Ready {
            first_frame_sequence: Some(1),
        },
        LifecycleEvent::BackgroundReady {
            title: "KOReader".into(),
            subtitle: "page 9".into(),
            resume_payload: Some(serde_json::json!({"page": 9})),
        },
        LifecycleEvent::StateSaved {
            resume_payload: Some(serde_json::json!({"page": 9})),
        },
        LifecycleEvent::ShutdownComplete { exit_code: 0 },
        LifecycleEvent::Failed {
            stage: LifecycleStage::Runtime,
            message: "failed".into(),
            retryable: true,
        },
        LifecycleEvent::Notification {
            title: "Title".into(),
            body: "Body".into(),
        },
    ];
    for event in events {
        let body = LifecycleEventBody {
            token: token(),
            event,
        };
        assert!(body.validate().is_ok());
        let value = serde_json::to_vec(&body).unwrap();
        let decoded: LifecycleEventBody = serde_json::from_slice(&value).unwrap();
        assert_eq!(decoded, body);
    }
}

#[test]
fn v1_foreground_conversion_keeps_the_v2_fence() {
    let body = LifecycleCommandBody {
        token: token(),
        command: LifecycleCommand::EnterForeground {
            resume_payload: None,
            open_path: None,
        },
    };
    assert!(matches!(
        crate::AppCommand::try_from(&body).unwrap(),
        crate::AppCommand::EnterForeground {
            foreground_epoch: Some(2),
            lease_id: Some(88),
            ..
        }
    ));
}
