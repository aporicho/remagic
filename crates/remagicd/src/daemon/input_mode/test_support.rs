use super::*;
use remagic_core::{AppManifest, ManagerState, ManifestStore, SessionStore};
use std::sync::atomic::{AtomicBool, AtomicU64};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

pub(in crate::daemon) fn manifest() -> AppManifest {
    toml::from_str(include_str!("../../../../../manifests/magicpaper.toml")).unwrap()
}

pub(in crate::daemon) fn token(id: &AppId) -> AppToken {
    AppToken {
        app_id: id.clone(),
        generation: 17,
        foreground_epoch: 23,
        lease_id: Some(23),
    }
}

pub(in crate::daemon) fn daemon_with(manifest: AppManifest, foreground: AppId) -> Daemon {
    daemon_with_events(manifest, foreground).0
}

pub(in crate::daemon) fn daemon_with_events(
    manifest: AppManifest,
    foreground: AppId,
) -> (Daemon, tokio::sync::mpsc::Receiver<QueuedEvent>) {
    let id = manifest.id.clone();
    let token = token(&id);
    let dynamic_allowed = supports_dynamic_input_mode(&manifest);
    let direct_ink_allowed = has_capability(&manifest, DIRECT_INK_CAPABILITY);
    let background_execution = manifest.runtime.background_execution;
    let root = std::env::temp_dir().join(format!(
        "remagic-input-mode-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    let (events, event_rx) = tokio::sync::mpsc::channel(1);
    let (power_control, _power_rx) = std::sync::mpsc::channel();
    let daemon = Daemon {
        state: RwLock::new(ManagerState {
            domain: DomainState::Foreground(foreground),
            last_app: Some(id.clone()),
            sequence: 1,
        }),
        manifests: RwLock::new(BTreeMap::from([(id.clone(), manifest)])),
        sessions: RwLock::new(BTreeMap::new()),
        runtime_generations: RwLock::new(BTreeMap::from([(id.clone(), 17)])),
        runtime_background_execution: RwLock::new(BTreeMap::from([(
            id.clone(),
            background_execution,
        )])),
        runtime_foreground_fences: RwLock::new(BTreeMap::from([(id.clone(), (23, 23))])),
        runtime_input_modes: RwLock::new(BTreeMap::from([(
            id,
            RuntimeInputState {
                generation: token.generation,
                foreground_epoch: token.foreground_epoch,
                lease_id: token.lease_id.unwrap(),
                mode: InputMode::AnimationLocked,
                dynamic_allowed,
                direct_ink_allowed,
                pending: false,
            },
        )])),
        runtime_exit_reports: RwLock::new(BTreeMap::new()),
        runtime_missing_observations: RwLock::new(BTreeMap::new()),
        session_store: SessionStore::new(root.clone()),
        manifest_store: ManifestStore::new(root.join("manifests")),
        controller: crate::system::SystemController::new(),
        power: Arc::new(crate::power_manager::PowerManager::load()),
        transition_lock: Mutex::new(()),
        events,
        power_control: power_device::ControlSender::from_test_channel(power_control),
        next_generation: AtomicU64::new(1),
        next_foreground_epoch: AtomicU64::new(1),
        next_sleep_epoch: AtomicU64::new(1),
        sleep_transaction: sleep::SleepTransaction::default(),
        launch_interrupt_epoch: Arc::new(AtomicU64::new(1)),
        cover_closed: Arc::new(AtomicBool::new(false)),
        cover_resume_app: RwLock::new(None),
        manager_repair_pending: AtomicBool::new(false),
        domain_recovery_pending: AtomicBool::new(false),
    };
    (daemon, event_rx)
}
