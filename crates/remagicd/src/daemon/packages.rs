use super::*;
use remagic_core::{AppKind, DeviceProfile};
use remagic_package::{PackageError, PackageManager};
use remagic_protocol::{ControlErrorCode, ControlReply, ControlResponse, Envelope};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::Mutex;

fn package_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Daemon {
    pub(super) async fn install_bundle_reply(
        &self,
        request_id: String,
        bundle: PathBuf,
    ) -> ControlResponse {
        let _guard = package_lock().lock().await;
        let device = match DeviceProfile::detect() {
            Ok(device) => device,
            Err(error) => {
                return self
                    .package_error(request_id, PackageError::Compatibility(error.to_string()))
                    .await
            }
        };
        let manager = PackageManager::from_environment();
        let prepared = match tokio::task::spawn_blocking({
            let manager = manager.clone();
            let device = device.clone();
            move || manager.prepare(&bundle, &device)
        })
        .await
        {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return self.package_error(request_id, error).await,
            Err(error) => return self.package_internal(request_id, error.to_string()).await,
        };
        let app_id = prepared.app_id().clone();
        if self.manifests.read().await.contains_key(&app_id) {
            if let Err(error) = self.close(app_id.clone(), true).await {
                return self.package_internal(request_id, error).await;
            }
        }
        let outcome = match tokio::task::spawn_blocking(move || manager.install(prepared)).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => return self.package_error(request_id, error).await,
            Err(error) => return self.package_internal(request_id, error.to_string()).await,
        };
        if let Err(error) = self.reload_manifests().await {
            return self.package_internal(request_id, error).await;
        }
        self.package_success(
            request_id,
            format!(
                "installed {} {} ({})",
                outcome.app_id, outcome.version, outcome.content_id
            ),
        )
        .await
    }

    pub(super) async fn upgrade_bundle_reply(
        &self,
        request_id: String,
        expected_app_id: AppId,
        bundle: Option<PathBuf>,
    ) -> ControlResponse {
        let Some(bundle) = bundle else {
            return self
                .package_reply(
                    request_id,
                    ControlReply::Error {
                        code: ControlErrorCode::InvalidRequest,
                        message: "upgrade requires an explicit verified bundle".into(),
                        state_revision: Some(self.state.read().await.sequence),
                    },
                )
                .await;
        };
        let _guard = package_lock().lock().await;
        let device = match DeviceProfile::detect() {
            Ok(device) => device,
            Err(error) => {
                return self
                    .package_error(request_id, PackageError::Compatibility(error.to_string()))
                    .await
            }
        };
        let manager = PackageManager::from_environment();
        let prepared = match tokio::task::spawn_blocking({
            let manager = manager.clone();
            let device = device.clone();
            move || manager.prepare(&bundle, &device)
        })
        .await
        {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return self.package_error(request_id, error).await,
            Err(error) => return self.package_internal(request_id, error.to_string()).await,
        };
        if prepared.app_id() != &expected_app_id {
            return self
                .package_reply(
                    request_id,
                    ControlReply::Error {
                        code: ControlErrorCode::PackageInvalid,
                        message: format!(
                            "bundle contains {}, expected {expected_app_id}",
                            prepared.app_id()
                        ),
                        state_revision: Some(self.state.read().await.sequence),
                    },
                )
                .await;
        }
        if !self.manifests.read().await.contains_key(&expected_app_id) {
            return self
                .package_reply(
                    request_id,
                    ControlReply::Error {
                        code: ControlErrorCode::AppNotFound,
                        message: format!("application is not installed: {expected_app_id}"),
                        state_revision: Some(self.state.read().await.sequence),
                    },
                )
                .await;
        }
        if let Err(error) = self.close(expected_app_id, true).await {
            return self.package_internal(request_id, error).await;
        }
        let outcome = match tokio::task::spawn_blocking(move || manager.install(prepared)).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => return self.package_error(request_id, error).await,
            Err(error) => return self.package_internal(request_id, error.to_string()).await,
        };
        if let Err(error) = self.reload_manifests().await {
            return self.package_internal(request_id, error).await;
        }
        self.package_success(
            request_id,
            format!(
                "upgraded {} to {} ({})",
                outcome.app_id, outcome.version, outcome.content_id
            ),
        )
        .await
    }

    pub(super) async fn rollback_bundle_reply(
        &self,
        request_id: String,
        app_id: AppId,
        version: Option<String>,
    ) -> ControlResponse {
        let _guard = package_lock().lock().await;
        if !self.manifests.read().await.contains_key(&app_id) {
            return self
                .package_reply(
                    request_id,
                    ControlReply::Error {
                        code: ControlErrorCode::AppNotFound,
                        message: format!("application is not installed: {app_id}"),
                        state_revision: Some(self.state.read().await.sequence),
                    },
                )
                .await;
        }
        let device = match DeviceProfile::detect() {
            Ok(device) => device,
            Err(error) => {
                return self
                    .package_error(request_id, PackageError::Compatibility(error.to_string()))
                    .await
            }
        };
        if let Err(error) = self.close(app_id.clone(), true).await {
            return self.package_internal(request_id, error).await;
        }
        let manager = PackageManager::from_environment();
        let rollback_id = app_id.clone();
        let outcome = match tokio::task::spawn_blocking(move || {
            manager.rollback(&rollback_id, version.as_deref(), &device)
        })
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => return self.package_error(request_id, error).await,
            Err(error) => return self.package_internal(request_id, error.to_string()).await,
        };
        if let Err(error) = self.reload_manifests().await {
            return self.package_internal(request_id, error).await;
        }
        self.package_success(
            request_id,
            format!("rolled back {} to {}", outcome.app_id, outcome.version),
        )
        .await
    }

    pub(super) async fn uninstall_bundle_reply(
        &self,
        request_id: String,
        app_id: AppId,
        purge: bool,
    ) -> ControlResponse {
        let _guard = package_lock().lock().await;
        let manifest = self.manifests.read().await.get(&app_id).cloned();
        let Some(manifest) = manifest else {
            return self
                .package_reply(
                    request_id,
                    ControlReply::Error {
                        code: ControlErrorCode::AppNotFound,
                        message: format!("application is not installed: {app_id}"),
                        state_revision: Some(self.state.read().await.sequence),
                    },
                )
                .await;
        };
        if manifest.kind == AppKind::System {
            return self
                .package_reply(
                    request_id,
                    ControlReply::Error {
                        code: ControlErrorCode::PermissionDenied,
                        message: format!("system application cannot be uninstalled: {app_id}"),
                        state_revision: Some(self.state.read().await.sequence),
                    },
                )
                .await;
        }
        if let Err(error) = self.close(app_id.clone(), true).await {
            return self.package_internal(request_id, error).await;
        }
        let manager = PackageManager::from_environment();
        let uninstall_id = app_id.clone();
        let outcome = match tokio::task::spawn_blocking(move || {
            manager.uninstall(&uninstall_id, purge)
        })
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => return self.package_error(request_id, error).await,
            Err(error) => return self.package_internal(request_id, error.to_string()).await,
        };
        if let Err(error) = self.reload_manifests().await {
            return self.package_internal(request_id, error).await;
        }
        self.package_success(
            request_id,
            format!(
                "uninstalled {}{}",
                outcome.app_id,
                if outcome.data_purged {
                    " and purged data"
                } else {
                    ""
                }
            ),
        )
        .await
    }

    async fn package_success(&self, request_id: String, output: String) -> ControlResponse {
        self.package_reply(
            request_id,
            ControlReply::PackageOutput {
                success: true,
                output,
                state_revision: self.state.read().await.sequence,
            },
        )
        .await
    }

    async fn package_internal(&self, request_id: String, message: String) -> ControlResponse {
        self.package_reply(
            request_id,
            ControlReply::Error {
                code: ControlErrorCode::Internal,
                message,
                state_revision: Some(self.state.read().await.sequence),
            },
        )
        .await
    }

    async fn package_error(&self, request_id: String, error: PackageError) -> ControlResponse {
        let code = match error {
            PackageError::Compatibility(_) => ControlErrorCode::UnsupportedDevice,
            PackageError::SystemApp(_) => ControlErrorCode::PermissionDenied,
            PackageError::Io(_, _) | PackageError::State(_) => ControlErrorCode::Internal,
            _ => ControlErrorCode::PackageInvalid,
        };
        self.package_reply(
            request_id,
            ControlReply::Error {
                code,
                message: error.to_string(),
                state_revision: Some(self.state.read().await.sequence),
            },
        )
        .await
    }

    async fn package_reply(&self, request_id: String, body: ControlReply) -> ControlResponse {
        Envelope::new(request_id, body)
    }
}
