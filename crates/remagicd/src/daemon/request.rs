use super::*;
use remagic_core::{BackgroundService, DomainState, SessionStatus, Transition};
use remagic_protocol::{AppView, Request, Response};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

impl Daemon {
    pub(super) async fn request(&self, request: Request) -> Response {
        match request {
            Request::Status => self.status_response().await,
            Request::ListApps => Response::Apps {
                apps: self.app_views().await,
            },
            Request::ReloadManifests => self.enqueue(Event::ReloadManifests).await,
            Request::OpenManager => self.enqueue(Event::OpenManager).await,
            Request::ReturnSystem => self.enqueue(Event::ReturnSystem).await,
            Request::Sleep => self.enqueue(Event::Sleep).await,
            Request::Launch { app_id, open_path } => {
                self.enqueue(Event::Launch(app_id, open_path)).await
            }
            Request::ParkCurrent => self.enqueue(Event::SinglePower).await,
            Request::Close { app_id, complete } => {
                self.enqueue(Event::Close(app_id, complete)).await
            }
            Request::RuntimeExited {
                app_id,
                generation,
                exit_code,
                crashed,
            } => {
                self.enqueue_runner_exit(app_id, generation, exit_code, crashed)
                    .await
            }
            Request::Ready { app_id } => self.enqueue(Event::AppReady(app_id)).await,
            Request::Parked {
                app_id,
                title,
                subtitle,
                resume_payload,
            } => {
                self.enqueue(Event::AppParked(AppSession {
                    schema: 1,
                    app_id,
                    status: SessionStatus::Parked,
                    title,
                    subtitle,
                    resume_payload,
                    updated_at: utils::unix_now(),
                    last_error: None,
                }))
                .await
            }
            Request::Notify {
                app_id,
                title,
                body,
            } => {
                info!(%app_id, %title, %body, "application notification queued");
                Response::Ok
            }
            Request::Package { operation } => self.enqueue(Event::Package(operation)).await,
        }
    }

    async fn status_response(&self) -> Response {
        let state = self.state.read().await;
        Response::Status {
            domain: state.domain.clone(),
            last_app: state.last_app.clone(),
            sequence: state.sequence,
        }
    }

    pub(super) async fn enqueue(&self, event: Event) -> Response {
        let timeout = self.request_timeout(&event).await;
        let deadline = tokio::time::Instant::now() + timeout;
        let (reply_tx, mut reply_rx) = tokio::sync::oneshot::channel();
        let request_fence = Arc::new(RequestFence::pending());
        let queued = QueuedEvent::new(
            event,
            Some(reply_tx),
            request_fence.clone(),
            &self.launch_interrupt_epoch,
        );
        match tokio::time::timeout_at(deadline, self.events.send(queued)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                request_fence.cancel();
                return Response::Error {
                    message: "manager event loop is unavailable".into(),
                };
            }
            Err(_) => {
                request_fence.cancel();
                return Response::Error {
                    message: "manager request timed out before it could be queued".into(),
                };
            }
        }
        match tokio::time::timeout_at(deadline, &mut reply_rx).await {
            Ok(reply) => event_reply(reply),
            Err(_) if request_fence.cancel() => Response::Error {
                message: "manager request timed out and was cancelled".into(),
            },
            Err(_) if request_fence.is_committing() => {
                match tokio::time::timeout(Duration::from_secs(5), reply_rx).await {
                    Ok(reply) => event_reply(reply),
                    Err(_) => Response::Error {
                        message: "manager request began committing but acknowledgement timed out"
                            .into(),
                    },
                }
            }
            Err(_) => Response::Error {
                message: "manager request was already cancelled".into(),
            },
        }
    }

    async fn request_timeout(&self, event: &Event) -> Duration {
        match event {
            Event::Launch(app_id, _) => {
                let readiness = self
                    .manifests
                    .read()
                    .await
                    .get(app_id)
                    .map_or(15_000, remagic_core::AppManifest::startup_timeout_ms);
                Duration::from_millis(readiness.saturating_add(10_000))
            }
            Event::Package(_) => Duration::from_secs(180),
            Event::ReturnSystem | Event::TriplePower | Event::Close(_, _) => {
                Duration::from_secs(45)
            }
            _ => Duration::from_secs(30),
        }
    }

    async fn app_views(&self) -> Vec<AppView> {
        let state = self.state.read().await.clone();
        let manifests: Vec<_> = self.manifests.read().await.values().cloned().collect();
        let sessions = self.sessions.read().await.clone();
        let mut views = Vec::with_capacity(manifests.len());
        for manifest in manifests {
            let background = manifest.effective_background_service();
            let background_active = match &background {
                Some(BackgroundService::Systemd { unit }) => self.controller.is_active(unit).await,
                Some(BackgroundService::Managed { .. }) | None => false,
            };
            views.push(AppView {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                installed: manifest.exec.exists() && manifest.working_dir.exists(),
                foreground: matches!(&state.domain, DomainState::Foreground(id) if id == &manifest.id),
                background_service: background.map(|service| match service {
                    BackgroundService::Systemd { unit } => unit,
                    BackgroundService::Managed { .. } => "remagic-managed".into(),
                }),
                background_active,
                session: sessions.get(&manifest.id).cloned(),
                package: manifest.package.clone(),
            });
        }
        views
    }

    pub(super) async fn handle_event(
        &self,
        event: Event,
        interrupt_epoch: u64,
        request_fence: Arc<RequestFence>,
    ) -> Result<(), String> {
        match event {
            Event::SinglePower => self.single_power(interrupt_epoch, &request_fence).await,
            Event::TriplePower => self.triple_power().await,
            // EVIOCGRAB means the stock shell cannot receive the press while
            // the managed domain owns input. Return ownership first; a long
            // press must also preempt any in-flight cold application launch.
            Event::LongPower => self.restore_system().await,
            Event::Launch(id, path) => self.launch(id, path, interrupt_epoch, &request_fence).await,
            Event::OpenManager => self.open_manager().await,
            Event::EnsureManager => self.handle_ensure_manager().await,
            Event::ReturnSystem => self.restore_system().await,
            Event::Sleep => self.sleep().await,
            Event::Close(id, complete) => self.close(id, complete).await,
            Event::RuntimeExited {
                app_id,
                generation,
                exit_code,
                crashed,
                source,
            } => {
                self.record_runtime_exit(app_id, generation, exit_code, crashed, source)
                    .await
            }
            Event::DisplayHostExited => {
                let result = self.restore_system().await;
                self.domain_recovery_pending.store(false, Ordering::Release);
                result
            }
            Event::AppReady(id) => self.record_app_ready(id).await,
            Event::AppParked(session) => self.record_parked(session).await,
            Event::Package(operation) => self.package(operation).await,
            Event::ReloadManifests => self.reload_manifests().await,
        }
    }

    async fn handle_ensure_manager(&self) -> Result<(), String> {
        let result = self.ensure_manager_surface().await;
        self.manager_repair_pending.store(false, Ordering::Release);
        result
    }

    async fn record_app_ready(&self, id: AppId) -> Result<(), String> {
        let mut state = self.state.write().await;
        if !matches!(&state.domain, DomainState::Launching(expected) if expected == &id) {
            warn!(%id, domain = ?state.domain, "ignored stale application-ready event");
            return Ok(());
        }
        state
            .apply(Transition::AppReady(id.clone()))
            .map_err(|e| e.to_string())?;
        if let DomainState::Foreground(id) = &state.domain {
            utils::set_foreground_marker(Some(id))?;
        }
        Ok(())
    }

    async fn reload_manifests(&self) -> Result<(), String> {
        let manifests = self
            .manifest_store
            .load_all()
            .map_err(|error| error.to_string())?;
        *self.manifests.write().await = manifests;
        if matches!(self.state.read().await.domain, DomainState::Manager) {
            if let Err(error) = self.restart_runtime_and_wait().await {
                warn!(%error, "runtime reload failed; restoring stock interface");
                return self.restore_system().await;
            }
        }
        Ok(())
    }

    async fn package(&self, operation: PackageOperation) -> Result<(), String> {
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("package operations are only available in the manager".into());
        }
        let encoded = serde_json::to_string(&operation).map_err(|e| e.to_string())?;
        self.controller
            .start_transient_worker("remagic-update", &[encoded])
            .await
    }

    async fn enqueue_runner_exit(
        &self,
        app_id: AppId,
        generation: u64,
        exit_code: i32,
        crashed: bool,
    ) -> Response {
        let pending = PendingExit {
            generation,
            source: ExitReportSource::Runner,
        };
        let reserved = {
            let mut reports = self.runtime_exit_reports.write().await;
            reserve_runner_report(&mut reports, app_id.clone(), pending)
        };
        if !reserved {
            // A duplicate callback for the same generation must not be able to
            // remove or race the reservation owned by the first callback.
            return Response::Ok;
        }
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let event = Event::RuntimeExited {
            app_id: app_id.clone(),
            generation,
            exit_code,
            crashed,
            source: ExitReportSource::Runner,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let send = self.events.send(QueuedEvent::non_cancellable(
            // Runtime exit evidence is an internal supervisor fact, not a
            // user-cancellable command. Once queued, it must eventually
            // consume this reservation even if acknowledgement times out.
            event,
            Some(reply_tx),
            &self.launch_interrupt_epoch,
        ));
        let send_failure = match tokio::time::timeout_at(deadline, send).await {
            Ok(Ok(())) => None,
            Ok(Err(_)) => Some("manager event loop is unavailable"),
            Err(_) => Some("runtime exit could not be queued before its deadline"),
        };
        if let Some(message) = send_failure {
            let mut reports = self.runtime_exit_reports.write().await;
            if reports.get(&app_id) == Some(&pending) {
                reports.remove(&app_id);
            }
            return Response::Error {
                message: message.into(),
            };
        }
        match tokio::time::timeout_at(deadline, reply_rx).await {
            Ok(Ok(Ok(()))) => Response::Ok,
            Ok(Ok(Err(message))) => Response::Error { message },
            Ok(Err(_)) => Response::Error {
                message: "manager event loop dropped the request".into(),
            },
            Err(_) => Response::Error {
                message: "runtime exit was accepted but acknowledgement timed out".into(),
            },
        }
    }
}

fn event_reply(
    reply: Result<Result<(), String>, tokio::sync::oneshot::error::RecvError>,
) -> Response {
    match reply {
        Ok(Ok(())) => Response::Ok,
        Ok(Err(message)) => Response::Error { message },
        Err(_) => Response::Error {
            message: "manager event loop dropped the request".into(),
        },
    }
}

fn reserve_runner_report(
    reports: &mut BTreeMap<AppId, PendingExit>,
    app_id: AppId,
    pending: PendingExit,
) -> bool {
    if reports.get(&app_id) == Some(&pending) {
        return false;
    }
    reports.insert(app_id, pending);
    true
}

#[cfg(test)]
mod runner_exit_tests {
    use super::*;

    #[test]
    fn duplicate_runner_callback_cannot_steal_the_first_reservation() {
        let id = AppId::new("magicpaper").unwrap();
        let runner = PendingExit {
            generation: 11,
            source: ExitReportSource::Runner,
        };
        let synthetic = PendingExit {
            generation: 11,
            source: ExitReportSource::Synthetic,
        };
        let mut reports = BTreeMap::from([(id.clone(), synthetic)]);
        assert!(reserve_runner_report(&mut reports, id.clone(), runner));
        assert!(!reserve_runner_report(&mut reports, id.clone(), runner));
        assert_eq!(reports.get(&id), Some(&runner));
    }
}
