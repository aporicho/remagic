use remagic_core::{AppId, AppManifest, AppToken, BackgroundExecution};
use std::path::PathBuf;

pub(super) struct LaunchContext {
    pub(super) id: AppId,
    pub(super) manifest: AppManifest,
    pub(super) open_path: Option<PathBuf>,
    pub(super) resume_payload: Option<serde_json::Value>,
    pub(super) runtime_dir: PathBuf,
    pub(super) unit: String,
    pub(super) active: bool,
    pub(super) generation: u64,
    pub(super) background_execution: BackgroundExecution,
    pub(super) foreground_epoch: u64,
    pub(super) lease_id: u64,
    pub(super) surface_key: i32,
    pub(super) launch_path: PathBuf,
    pub(super) background_quiesced: bool,
}

impl LaunchContext {
    pub(super) fn token(&self) -> AppToken {
        AppToken {
            app_id: self.id.clone(),
            generation: self.generation,
            foreground_epoch: self.foreground_epoch,
            lease_id: Some(self.lease_id),
        }
    }
}
