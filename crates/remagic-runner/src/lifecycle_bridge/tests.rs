use super::*;
use remagic_core::{
    AppId, AppToken, Capability, CertificatePolicy, FontPolicy, LaunchEnvironment, LocalePolicy,
    NetworkPolicy, RuntimeDirectories, RuntimeProfile, TimezonePolicy,
};
use remagic_protocol::{
    read_frame, write_frame, AppCommand, Envelope, LifecycleCommand, LifecycleCommandBody,
    LifecycleCommandEnvelope, LifecycleEvent, LifecycleEventBody, LifecycleStage, Response,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::unix::AsyncFd;
use tokio::net::{UnixListener, UnixStream};

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);
static NEXT_STATUS: AtomicU64 = AtomicU64::new(1);

fn token() -> AppToken {
    AppToken {
        app_id: AppId::new("magicpaper").unwrap(),
        generation: 7,
        foreground_epoch: 2,
        lease_id: Some(11),
    }
}

fn environment() -> LaunchEnvironment {
    let root = PathBuf::from("/run/remagic/apps/magicpaper");
    let mut variables = BTreeMap::new();
    for (key, value) in [
        ("HOME", "/home/root"),
        ("XDG_CONFIG_HOME", "/home/root/.config/magicpaper"),
        ("XDG_DATA_HOME", "/home/root/.local/share/magicpaper"),
        ("XDG_STATE_HOME", "/home/root/.local/state/magicpaper"),
        ("XDG_CACHE_HOME", "/home/root/.cache/magicpaper"),
        ("XDG_RUNTIME_DIR", "/run/remagic/apps/magicpaper"),
        ("LANG", "C.UTF-8"),
        ("TZ", "UTC"),
        ("PATH", "/usr/bin:/bin"),
        ("REMAGIC_APP_ID", "magicpaper"),
        ("REMAGIC_RUNTIME_PROFILE", "qtfb_compat"),
        ("REMAGIC_NETWORK_POLICY_MODE", "deny"),
        ("REMAGIC_NETWORK_POLICY_ENFORCEMENT", "metadata_only"),
        ("REMAGIC_NETWORK_ISOLATED", "0"),
        ("REMAGIC_NETWORK_ALLOWED_HOSTS", ""),
    ] {
        variables.insert(key.to_owned(), value.to_owned());
    }
    LaunchEnvironment {
        app_id: AppId::new("magicpaper").unwrap(),
        profile: RuntimeProfile::QtfbCompat,
        directories: RuntimeDirectories {
            home: PathBuf::from("/home/root"),
            config_home: PathBuf::from("/home/root/.config/magicpaper"),
            data_home: PathBuf::from("/home/root/.local/share/magicpaper"),
            state_home: PathBuf::from("/home/root/.local/state/magicpaper"),
            cache_home: PathBuf::from("/home/root/.cache/magicpaper"),
            runtime_dir: root,
        },
        variables,
        resolved_libraries: Vec::new(),
        platform_capabilities: BTreeSet::<Capability>::new(),
        locale: LocalePolicy::default(),
        timezone: TimezonePolicy::default(),
        fonts: FontPolicy::default(),
        certificates: CertificatePolicy::default(),
        network: NetworkPolicy::default(),
    }
}

fn command_envelope() -> LifecycleCommandEnvelope {
    Envelope::new(
        "test-command",
        LifecycleCommandBody {
            token: token(),
            command: LifecycleCommand::Start {
                launch_environment: Box::new(environment()),
                resume_payload: None,
                open_path: None,
            },
        },
    )
}

fn descriptor_pair() -> (OwnedFd, OwnedFd) {
    let mut descriptors = [-1; 2];
    assert_eq!(
        unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
                descriptors.as_mut_ptr(),
            )
        },
        0
    );
    unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    }
}

fn status_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "remagic-runner-status-{label}-{}-{}",
        std::process::id(),
        NEXT_STATUS.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn magicpaper_uses_canonical_length_prefixed_protocol() {
    let packet = encode_command(ChildTransport::LengthPrefixed, &command_envelope()).unwrap();
    let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
    assert_eq!(length, packet.len() - 4);
    let value: Value = serde_json::from_slice(&packet[4..]).unwrap();
    assert_eq!(value["body"]["token"]["generation"], 7);
    assert_eq!(value["body"]["command"], "start");
}

#[test]
fn koreader_translation_is_newline_json_with_flat_fence() {
    let mut envelope = command_envelope();
    envelope.body.token.app_id = AppId::new("koreader").unwrap();
    envelope.body.command = LifecycleCommand::EnterBackground;
    let packet = encode_command(ChildTransport::Newline, &envelope).unwrap();
    assert!(packet.ends_with(b"\n"));
    let value: Value = serde_json::from_slice(&packet[..packet.len() - 1]).unwrap();
    assert_eq!(value["body"]["app_id"], "koreader");
    assert_eq!(value["body"]["generation"], 7);
    assert_eq!(value["body"]["foreground_epoch"], 2);
    assert_eq!(value["body"]["lease_id"], 11);
    assert!(value["body"].get("token").is_none());
}

#[test]
fn koreader_compatibility_event_becomes_canonical_event() {
    let envelope = decode_event(
        br#"{"protocol":2,"request_id":"ko-ready","body":{"event":"ready","app_id":"koreader","generation":9,"foreground_epoch":3,"lease_id":17,"ui":"filemanager"}}"#,
    )
    .unwrap();
    assert_eq!(envelope.body.token.app_id.as_str(), "koreader");
    assert_eq!(envelope.body.token.generation, 9);
    assert!(matches!(envelope.body.event, LifecycleEvent::Ready { .. }));
}

#[test]
fn malformed_packets_and_stale_shapes_are_rejected() {
    assert!(decode_packet(ChildTransport::LengthPrefixed, b"\0\0\0\x05x").is_err());
    assert!(decode_packet(ChildTransport::Newline, b"{}").is_err());
    assert!(decode_event(
        br#"{"protocol":2,"request_id":"bad","body":{"event":"ready","app_id":"koreader","generation":0}}"#
    )
    .is_err());
}

#[tokio::test]
async fn start_uses_the_daemon_supplied_initial_token() {
    let (parent, child) = descriptor_pair();
    let bridge =
        LifecycleBridge::new(parent, AppId::new("magicpaper").unwrap(), 71, 23, 47, 3_500).unwrap();
    bridge.send_start(environment(), None, None).await.unwrap();
    let child = AsyncFd::new(child).unwrap();
    let packet = receive_packet(&child).await.unwrap();
    let payloads = decode_packet(ChildTransport::LengthPrefixed, &packet).unwrap();
    let envelope: LifecycleCommandEnvelope = serde_json::from_slice(&payloads[0]).unwrap();
    assert_eq!(envelope.body.token.generation, 71);
    assert_eq!(envelope.body.token.foreground_epoch, 23);
    assert_eq!(envelope.body.token.lease_id, Some(47));
    assert!(matches!(
        envelope.body.command,
        LifecycleCommand::Start { .. }
    ));
}

#[tokio::test]
async fn daemon_control_uses_app_command_framing_and_updates_the_token() {
    let (parent, child) = descriptor_pair();
    let bridge =
        LifecycleBridge::new(parent, AppId::new("magicpaper").unwrap(), 7, 9, 11, 3_500).unwrap();
    let socket_path = std::env::temp_dir().join(format!(
        "remagic-runner-control-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server_bridge = bridge.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve_control_client(stream, server_bridge, unsafe { libc::geteuid() }).await
    });

    let mut client = UnixStream::connect(&socket_path).await.unwrap();
    write_frame(
        &mut client,
        &AppCommand::EnterForeground {
            resume_payload: Some(serde_json::json!({"page": 4})),
            open_path: None,
            foreground_epoch: Some(42),
            lease_id: Some(77),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read_frame::<_, Response>(&mut client).await.unwrap(),
        Response::Ok
    );
    server.await.unwrap().unwrap();

    let child = AsyncFd::new(child).unwrap();
    let packet = receive_packet(&child).await.unwrap();
    let payloads = decode_packet(ChildTransport::LengthPrefixed, &packet).unwrap();
    let envelope: LifecycleCommandEnvelope = serde_json::from_slice(&payloads[0]).unwrap();
    assert_eq!(envelope.body.token.foreground_epoch, 42);
    assert_eq!(envelope.body.token.lease_id, Some(77));
    assert!(matches!(
        envelope.body.command,
        LifecycleCommand::EnterForeground { .. }
    ));
    fs::remove_file(socket_path).unwrap();
}

#[test]
fn explicit_foreground_fences_must_be_complete_nonzero_and_strictly_newer() {
    let mut cursor = TokenCursor::new(AppId::new("magicpaper").unwrap(), 5, 8, 13);
    let explicit = cursor.foreground_with_fence(Some(41), Some(77)).unwrap();
    assert_eq!(explicit.foreground_epoch, 41);
    assert_eq!(explicit.lease_id, Some(77));
    assert!(matches!(
        cursor.foreground_with_fence(Some(41), Some(78)),
        Err(BridgeError::StaleForegroundEpoch {
            current: 41,
            requested: 41
        })
    ));
    assert!(matches!(
        cursor.foreground_with_fence(Some(42), None),
        Err(BridgeError::IncompleteForegroundFence)
    ));
    assert!(matches!(
        cursor.foreground_with_fence(Some(42), Some(0)),
        Err(BridgeError::InvalidForegroundFence)
    ));
}

#[tokio::test]
async fn current_event_is_atomically_published_and_stale_event_cannot_replace_it() {
    let root = status_root("current");
    for stale in [
        "lifecycle-status.json",
        "koreader-ready",
        "koreader-exit",
        ".lifecycle-status.old.tmp",
        ".koreader-ready.old.tmp",
    ] {
        fs::write(root.join(stale), b"stale").unwrap();
    }
    fs::write(root.join("application-data"), b"keep").unwrap();
    let store = LifecycleStatusStore::new(root.clone());
    store.clear_stale().unwrap();
    assert!(root.join("application-data").is_file());
    assert!(!root.join("koreader-ready").exists());

    let (parent, _child) = descriptor_pair();
    let bridge =
        LifecycleBridge::new(parent, AppId::new("magicpaper").unwrap(), 7, 2, 11, 3_500).unwrap();
    let ready = Envelope::new(
        "ready-current",
        LifecycleEventBody {
            token: token(),
            event: LifecycleEvent::Ready {
                first_frame_sequence: Some(19),
            },
        },
    );
    assert!(bridge.persist_current_event(&store, &ready).await.unwrap());
    let status: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(status["generation"], 7);
    assert_eq!(status["foreground_epoch"], 2);
    assert_eq!(status["lease_id"], 11);
    assert_eq!(status["event"], "ready");
    assert_eq!(status["first_frame_sequence"], 19);
    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let mut stale_token = token();
    stale_token.foreground_epoch = 1;
    let stale = Envelope::new(
        "ready-stale",
        LifecycleEventBody {
            token: stale_token,
            event: LifecycleEvent::Ready {
                first_frame_sequence: Some(20),
            },
        },
    );
    assert!(!bridge.persist_current_event(&store, &stale).await.unwrap());
    let mut missing_lease_token = token();
    missing_lease_token.lease_id = None;
    let missing_lease = Envelope::new(
        "ready-missing-lease",
        LifecycleEventBody {
            token: missing_lease_token,
            event: LifecycleEvent::Ready {
                first_frame_sequence: Some(21),
            },
        },
    );
    assert!(!bridge
        .persist_current_event(&store, &missing_lease)
        .await
        .unwrap());
    let unchanged: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(unchanged["request_id"], "ready-current");
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".lifecycle-status.")
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn status_file_preserves_payloads_for_every_semantic_event() {
    let root = status_root("payloads");
    let store = LifecycleStatusStore::new(root.clone());
    let cases = [
        (
            LifecycleEvent::BackgroundReady {
                title: "MagicPaper".into(),
                subtitle: "page 4".into(),
                resume_payload: Some(serde_json::json!({"page": 4})),
            },
            "background_ready",
            "resume_payload",
        ),
        (
            LifecycleEvent::StateSaved {
                resume_payload: Some(serde_json::json!({"page": 5})),
            },
            "state_saved",
            "resume_payload",
        ),
        (
            LifecycleEvent::Failed {
                stage: LifecycleStage::Background,
                message: "save failed".into(),
                retryable: true,
            },
            "failed",
            "message",
        ),
        (
            LifecycleEvent::ShutdownComplete { exit_code: 0 },
            "shutdown_complete",
            "exit_code",
        ),
    ];
    for (index, (event, expected_event, required_field)) in cases.into_iter().enumerate() {
        let envelope = Envelope::new(
            format!("event-{index}"),
            LifecycleEventBody {
                token: token(),
                event,
            },
        );
        store.write(&envelope).unwrap();
        let status: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(status["event"], expected_event);
        assert!(status.get(required_field).is_some());
    }
    let status: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(status["state_saved"], true);
    assert_eq!(status["background_ready"], true);
    assert_eq!(status["title"], "MagicPaper");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn foreground_commands_allocate_monotonic_epoch_and_nonzero_lease() {
    let mut cursor = TokenCursor::new(AppId::new("magicpaper").unwrap(), 5, 8, 13);
    assert_eq!(cursor.token.foreground_epoch, 8);
    assert_eq!(cursor.token.lease_id, Some(13));
    let first = cursor.foreground().unwrap();
    let second = cursor.foreground().unwrap();
    assert_eq!(first.foreground_epoch, 9);
    assert_eq!(second.foreground_epoch, 10);
    assert_ne!(first.lease_id, second.lease_id);
    assert_ne!(first.lease_id, Some(0));
}
