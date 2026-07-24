use crate::bootstrap::PreparedApplication;
use crate::lifecycle_bridge::{BridgeError, LifecycleBridge, LifecycleStatusStore};
use crate::process::{graceful_stop, RunningApplication};
use remagic_core::{AppId, MANIFEST_SCHEMA_V2};
use remagic_protocol::{read_frame, write_frame, LifecycleEvent, Request, Response};
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

const CALLBACK_TIMEOUT: Duration = Duration::from_millis(500);
const LIFECYCLE_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) async fn supervise(
    running: RunningApplication,
) -> Result<(), Box<dyn std::error::Error>> {
    let RunningApplication {
        mut prepared,
        mut child,
    } = running;
    let mut tasks = AuxiliaryTasks::spawn(&mut prepared);
    if prepared.lifecycle.bridge.is_none() {
        send_callback(
            Request::Ready {
                app_id: prepared.id.clone(),
            },
            "legacy ready",
        )
        .await;
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = terminate.recv() => {
            graceful_stop(
                &mut child,
                prepared.lifecycle.bridge.as_ref(),
                &prepared.manifest.shutdown,
            ).await?
        }
    };
    tasks.stop().await;
    // Reporting exact exit status is useful even for an out-of-band SIGTERM,
    // but it must never become an exit dependency. Every callback below has
    // a hard connect+write+ack deadline, so a daemon blocked in systemctl stop
    // cannot form a circular wait with this runner.
    publish_exit(&prepared, &status).await;
    finish(status)
}

struct AuxiliaryTasks {
    control: Option<JoinHandle<()>>,
    lifecycle: Option<JoinHandle<()>>,
}

impl AuxiliaryTasks {
    fn spawn(prepared: &mut PreparedApplication) -> Self {
        let bridge = prepared.lifecycle.bridge.clone();
        let control = prepared
            .lifecycle
            .control_socket
            .take()
            .zip(bridge.clone())
            .map(|(socket, bridge)| {
                tokio::spawn(async move {
                    if let Err(error) = socket.run(bridge).await {
                        eprintln!("remagic-runner: application control socket stopped: {error}");
                    }
                })
            });
        let lifecycle = prepared
            .lifecycle
            .status_store
            .take()
            .zip(bridge)
            .map(|(status, bridge)| spawn_lifecycle_task(bridge, status, prepared.id.clone()));
        Self { control, lifecycle }
    }

    async fn stop(&mut self) {
        if let Some(task) = self.control.take() {
            task.abort();
        }
        if let Some(mut task) = self.lifecycle.take() {
            if tokio::time::timeout(LIFECYCLE_DRAIN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
    }
}

fn spawn_lifecycle_task(
    bridge: LifecycleBridge,
    status: LifecycleStatusStore,
    app_id: AppId,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        match forward_lifecycle_events(bridge, status, app_id).await {
            Ok(()) | Err(BridgeError::Disconnected) => {}
            Err(error) => eprintln!("remagic-runner: lifecycle event bridge stopped: {error}"),
        }
    })
}

async fn forward_lifecycle_events(
    bridge: LifecycleBridge,
    status: LifecycleStatusStore,
    app_id: AppId,
) -> Result<(), BridgeError> {
    loop {
        for envelope in bridge.receive_events().await? {
            if !bridge.persist_current_event(&status, &envelope).await? {
                log_stale_event(&envelope);
                continue;
            }
            handle_lifecycle_event(&app_id, envelope.body.event).await;
        }
    }
}

fn log_stale_event(envelope: &remagic_protocol::LifecycleEventEnvelope) {
    eprintln!(
        "remagic-runner: ignored stale lifecycle event for {} generation {} epoch {}",
        envelope.body.token.app_id,
        envelope.body.token.generation,
        envelope.body.token.foreground_epoch
    );
}

async fn handle_lifecycle_event(app_id: &AppId, event: LifecycleEvent) {
    match event {
        LifecycleEvent::Ready {
            first_frame_sequence,
        } => {
            eprintln!(
                "remagic-runner: application {app_id} ready (first_frame={first_frame_sequence:?})"
            );
        }
        LifecycleEvent::BackgroundReady { .. } => {
            eprintln!("remagic-runner: application {app_id} background-ready");
        }
        LifecycleEvent::StateSaved { .. } => {
            eprintln!("remagic-runner: application {app_id} state saved");
        }
        LifecycleEvent::ShutdownComplete { exit_code } => {
            eprintln!("remagic-runner: application {app_id} shutdown complete ({exit_code})");
        }
        LifecycleEvent::Failed {
            stage,
            message,
            retryable,
        } => {
            eprintln!(
                "remagic-runner: application {app_id} failed at {stage:?}: \
                 {message} (retryable={retryable})"
            );
        }
        LifecycleEvent::Notification { title, body } => {
            send_callback(
                Request::Notify {
                    app_id: app_id.clone(),
                    title,
                    body,
                },
                "notification",
            )
            .await;
        }
    }
}

async fn publish_exit(prepared: &PreparedApplication, status: &ExitStatus) {
    let exit_code = status.code().unwrap_or(1);
    if prepared.manifest.schema == MANIFEST_SCHEMA_V2 {
        if let Some(generation) = prepared.plan.generation {
            send_callback(
                Request::RuntimeExited {
                    app_id: prepared.id.clone(),
                    generation,
                    exit_code,
                    crashed: !status.success(),
                },
                "runtime-exit",
            )
            .await;
        }
        return;
    }
    let subtitle = if status.success() {
        "已暂停，可继续".to_string()
    } else {
        format!("异常退出：{status}")
    };
    let resume_payload = prepared
        .open_path
        .as_ref()
        .map(|path| serde_json::json!({ "open_path": path }))
        .or_else(|| prepared.resume_payload.clone());
    send_callback(
        Request::Parked {
            app_id: prepared.id.clone(),
            title: prepared.manifest.name.clone(),
            subtitle,
            resume_payload,
        },
        "legacy exit",
    )
    .await;
}

async fn send_callback(request: Request, label: &str) {
    let _ = send_callback_to(Path::new(remagic_protocol::DEFAULT_SOCKET), request, label).await;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallbackOutcome {
    Acknowledged,
    Failed,
    TimedOut,
}

async fn send_callback_to(socket: &Path, request: Request, label: &str) -> CallbackOutcome {
    match tokio::time::timeout(CALLBACK_TIMEOUT, send_to(socket, request)).await {
        Ok(Ok(_)) => CallbackOutcome::Acknowledged,
        Ok(Err(error)) => {
            eprintln!("remagic-runner: {label} callback failed: {error}");
            CallbackOutcome::Failed
        }
        Err(_) => {
            eprintln!(
            "remagic-runner: {label} callback exceeded {} ms; supervised unit state remains authoritative",
                CALLBACK_TIMEOUT.as_millis()
            );
            CallbackOutcome::TimedOut
        }
    }
}

async fn send_to(socket: &Path, request: Request) -> Result<Response, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(socket).await?;
    write_frame(&mut stream, &request).await?;
    Ok(read_frame(&mut stream).await?)
}

fn finish(status: ExitStatus) -> Result<(), Box<dyn std::error::Error>> {
    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::future::pending;
    use std::time::Instant;
    use tokio::net::UnixListener;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn accepted_runtime_exit_callback_cannot_wait_forever_for_acknowledgement() {
        let socket = std::env::temp_dir().join(format!(
            "remagic-runner-callback-{}.sock",
            std::process::id()
        ));
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let (observed_tx, observed_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = read_frame(&mut stream).await.unwrap();
            let _ = observed_tx.send(request);
            pending::<()>().await;
        });
        let request = Request::RuntimeExited {
            app_id: AppId::new("magicpaper").unwrap(),
            generation: 73,
            exit_code: 0,
            crashed: false,
        };
        let started = Instant::now();
        let outcome = send_callback_to(&socket, request, "test runtime-exit").await;
        assert_eq!(outcome, CallbackOutcome::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            observed_rx.await.unwrap(),
            Request::RuntimeExited {
                generation: 73,
                exit_code: 0,
                crashed: false,
                ..
            }
        ));
        server.abort();
        let _ = fs::remove_file(socket);
    }
}
