use super::*;
use remagic_core::{
    runtime::NetworkEnforcement, BackgroundService, Capability, DeviceProfile, LaunchEnvironment,
    PreflightCheck, PreflightReport, PreflightStatus, SupervisorState,
};
use remagic_protocol::{
    AppViewV2, ControlErrorCode, ControlIntent, ControlReply, ControlRequest, ControlResponse,
    Request, Response, SupervisorSnapshot,
};
use std::collections::BTreeSet;

impl Daemon {
    pub(super) async fn control_v2(&self, request: ControlRequest) -> ControlResponse {
        let request_id = request.request_id.clone();
        if let Err(error) = request.validate_header() {
            return reply(
                request_id,
                ControlReply::Error {
                    code: ControlErrorCode::InvalidRequest,
                    message: error.to_string(),
                    state_revision: None,
                },
            );
        }
        let revision = self.state.read().await.sequence;
        if request
            .expected_state_revision
            .is_some_and(|expected| expected != revision)
        {
            return reply(
                request_id,
                ControlReply::Error {
                    code: ControlErrorCode::RevisionConflict,
                    message: format!(
                        "state revision changed: expected {:?}, actual {revision}",
                        request.expected_state_revision
                    ),
                    state_revision: Some(revision),
                },
            );
        }
        let body = match request.body {
            ControlIntent::Snapshot => ControlReply::Snapshot {
                snapshot: self.supervisor_snapshot().await,
            },
            ControlIntent::PowerSnapshot => ControlReply::Power {
                snapshot: self.power.snapshot().await,
                state_revision: revision,
            },
            ControlIntent::SetIdleSuspend { seconds } => {
                match self.power.set_idle_suspend(seconds).await {
                    Ok(_) => ControlReply::Power {
                        snapshot: self.power.snapshot().await,
                        state_revision: revision,
                    },
                    Err(message) => {
                        error_reply(ControlErrorCode::InvalidRequest, message, revision)
                    }
                }
            }
            ControlIntent::Subscribe { .. } => ControlReply::Subscribed {
                state_revision: revision,
            },
            ControlIntent::Preflight { app_id } => match self.preflight(&app_id).await {
                Ok(report) => ControlReply::Preflight {
                    report: Box::new(report),
                },
                Err(message) => error_reply(ControlErrorCode::PreflightFailed, message, revision),
            },
            ControlIntent::Install { bundle } => {
                return self.install_bundle_reply(request_id, bundle).await;
            }
            ControlIntent::Upgrade { app_id, bundle } => {
                return self.upgrade_bundle_reply(request_id, app_id, bundle).await;
            }
            ControlIntent::Rollback { app_id, version } => {
                return self
                    .rollback_bundle_reply(request_id, app_id, version)
                    .await;
            }
            ControlIntent::Uninstall { app_id, purge } => {
                return self.uninstall_bundle_reply(request_id, app_id, purge).await;
            }
            intent => match legacy_request(intent) {
                Ok(request) => map_v1_reply(
                    self.request(request).await,
                    self.state.read().await.sequence,
                ),
                Err(message) => error_reply(ControlErrorCode::InvalidRequest, message, revision),
            },
        };
        reply(request_id, body)
    }

    async fn supervisor_snapshot(&self) -> SupervisorSnapshot {
        let legacy = self.state.read().await.clone();
        let state = SupervisorState::from(legacy);
        let manifests = self
            .manifests
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let sessions = self.sessions.read().await.clone();
        let mut apps = Vec::with_capacity(manifests.len());
        for manifest in manifests {
            let background = manifest.effective_background_service();
            let background_unit = background.as_ref().map(|service| match service {
                BackgroundService::Systemd { unit } => unit.clone(),
                BackgroundService::Managed { .. } => {
                    crate::system::managed_background_unit(&manifest.id)
                }
            });
            let background_active = match background_unit.as_deref() {
                Some(unit) => self.controller.is_active(unit).await,
                None => false,
            };
            apps.push(AppViewV2 {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                version: manifest.version.clone(),
                kind: manifest.kind,
                installed: manifest.exec.is_file() && manifest.working_dir.is_dir(),
                runtime_profile: manifest.runtime_profile(),
                capabilities: manifest.capabilities.clone(),
                instance: state.apps.get(&manifest.id).cloned(),
                background_service: background_unit,
                background_active,
                session: sessions.get(&manifest.id).cloned(),
                package: manifest.package.clone(),
                supported_devices: manifest.supported_devices.clone(),
                supported_os: manifest.supported_os.clone(),
                required_remagic_api: manifest.required_remagic_api,
                uninstall_policy: manifest.uninstall_policy,
                preflight: None,
            });
        }
        SupervisorSnapshot { state, apps }
    }

    async fn preflight(&self, app_id: &AppId) -> Result<PreflightReport, String> {
        let manifest = self
            .manifests
            .read()
            .await
            .get(app_id)
            .cloned()
            .ok_or_else(|| format!("unknown application {app_id}"))?;
        let device = DeviceProfile::detect().map_err(|error| error.to_string())?;
        let mut checks = Vec::new();
        check(
            &mut checks,
            "device",
            manifest.supported_devices.is_empty()
                || manifest.supported_devices.contains(&device.product),
            format!("detected {:?}", device.product),
        );
        check(
            &mut checks,
            "os",
            manifest.supported_os.is_empty() || manifest.supported_os.contains(&device.os_version),
            format!("detected {}", device.os_version),
        );
        check(
            &mut checks,
            "executable",
            manifest.exec.is_file() && manifest.working_dir.is_dir(),
            manifest.exec.display().to_string(),
        );
        let platform_capabilities = device
            .capabilities
            .iter()
            .filter_map(|value| Capability::new(value.clone()).ok())
            .collect::<BTreeSet<_>>();
        let missing_capabilities = manifest
            .capabilities
            .iter()
            .filter(|capability| !platform_capabilities.contains(*capability))
            .cloned()
            .collect::<BTreeSet<_>>();
        let compatible = checks
            .iter()
            .all(|item| item.status != PreflightStatus::Failed)
            && missing_capabilities.is_empty();
        let launch_environment = if compatible {
            Some(
                LaunchEnvironment::resolve(
                    manifest.id.clone(),
                    &manifest.runtime,
                    &manifest.environment,
                    Vec::new(),
                    platform_capabilities,
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    NetworkEnforcement::MetadataOnly,
                    device,
                )
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let profile = manifest.runtime_profile();
        Ok(PreflightReport {
            app_id: manifest.id,
            profile,
            compatible,
            checks,
            missing_capabilities,
            missing_libraries: Vec::new(),
            launch_environment,
        })
    }
}

fn check(checks: &mut Vec<PreflightCheck>, id: &str, passed: bool, message: String) {
    checks.push(PreflightCheck {
        id: id.into(),
        status: if passed {
            PreflightStatus::Passed
        } else {
            PreflightStatus::Failed
        },
        message,
    });
}

fn legacy_request(intent: ControlIntent) -> Result<Request, String> {
    Ok(match intent {
        ControlIntent::ReloadManifests => Request::ReloadManifests,
        ControlIntent::ShowHome => Request::OpenManager,
        ControlIntent::ReturnStock => Request::ReturnSystem,
        ControlIntent::Launch { app_id, open_path } => Request::Launch { app_id, open_path },
        ControlIntent::OpenPath { app_id, path } => Request::Launch {
            app_id,
            open_path: Some(path),
        },
        ControlIntent::ParkCurrent => Request::ParkCurrent,
        ControlIntent::Close { app_id } => Request::Close {
            app_id,
            complete: true,
        },
        ControlIntent::LegacyPackage { operation } => Request::Package { operation },
        ControlIntent::Sleep | ControlIntent::Wake => {
            return Err("sleep and wake require a manager-rendered surface fence".into())
        }
        _ => return Err("intent is not available through the compatibility path".into()),
    })
}

fn map_v1_reply(response: Response, revision: u64) -> ControlReply {
    match response {
        Response::Ok => ControlReply::Ack {
            state_revision: revision,
        },
        Response::PackageOutput { success, output } => ControlReply::PackageOutput {
            success,
            output,
            state_revision: revision,
        },
        Response::Error { message } => error_reply(ControlErrorCode::Internal, message, revision),
        _ => error_reply(
            ControlErrorCode::Internal,
            "unexpected compatibility response".into(),
            revision,
        ),
    }
}

fn error_reply(code: ControlErrorCode, message: String, revision: u64) -> ControlReply {
    ControlReply::Error {
        code,
        message,
        state_revision: Some(revision),
    }
}

fn reply(request_id: String, body: ControlReply) -> ControlResponse {
    remagic_protocol::Envelope::new(request_id, body)
}
