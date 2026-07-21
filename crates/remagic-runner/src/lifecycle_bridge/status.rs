use super::BridgeError;
use remagic_core::AppToken;
use remagic_protocol::{LifecycleEvent, LifecycleEventEnvelope};
use serde_json::{Map, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct LifecycleStatusStore {
    runtime_dir: PathBuf,
    next_temporary: Arc<AtomicU64>,
    milestones: Arc<Mutex<Milestones>>,
}

#[derive(Default)]
struct Milestones {
    token: Option<AppToken>,
    state_saved: bool,
    background_ready: bool,
    title: Option<String>,
    subtitle: Option<String>,
    resume_payload: Option<Value>,
}

impl LifecycleStatusStore {
    pub(crate) fn new(runtime_dir: PathBuf) -> Self {
        Self {
            runtime_dir,
            next_temporary: Arc::new(AtomicU64::new(1)),
            milestones: Arc::new(Mutex::new(Milestones::default())),
        }
    }

    pub(crate) fn clear_stale(&self) -> io::Result<()> {
        fs::create_dir_all(&self.runtime_dir)?;
        for entry in fs::read_dir(&self.runtime_dir)? {
            let entry = entry?;
            if !is_stale_name(&entry.file_name().to_string_lossy()) {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_file() || file_type.is_symlink() || file_type.is_socket() {
                remove_if_present(entry.path())?;
            }
        }
        Ok(())
    }

    pub(super) fn write(&self, envelope: &LifecycleEventEnvelope) -> Result<(), BridgeError> {
        if matches!(envelope.body.event, LifecycleEvent::Notification { .. }) {
            return Ok(());
        }
        let mut value = status_value(envelope)?;
        self.enrich_milestones(envelope, &mut value);
        self.atomic_write(&value)?;
        Ok(())
    }

    fn enrich_milestones(&self, envelope: &LifecycleEventEnvelope, value: &mut Value) {
        let mut milestones = self.milestones.lock().unwrap();
        if milestones.token.as_ref() != Some(&envelope.body.token) {
            *milestones = Milestones {
                token: Some(envelope.body.token.clone()),
                ..Milestones::default()
            };
        }
        match &envelope.body.event {
            LifecycleEvent::StateSaved { resume_payload } => {
                milestones.state_saved = true;
                if resume_payload.is_some() {
                    milestones.resume_payload = resume_payload.clone();
                }
            }
            LifecycleEvent::BackgroundReady {
                title,
                subtitle,
                resume_payload,
            } => {
                milestones.background_ready = true;
                milestones.title = Some(title.clone());
                milestones.subtitle = Some(subtitle.clone());
                if resume_payload.is_some() {
                    milestones.resume_payload = resume_payload.clone();
                }
            }
            _ => {}
        }
        let Some(status) = value.as_object_mut() else {
            return;
        };
        status.insert("state_saved".into(), Value::from(milestones.state_saved));
        status.insert(
            "background_ready".into(),
            Value::from(milestones.background_ready),
        );
        insert_optional(status, "title", milestones.title.clone().map(Value::from));
        insert_optional(
            status,
            "subtitle",
            milestones.subtitle.clone().map(Value::from),
        );
        insert_optional(status, "resume_payload", milestones.resume_payload.clone());
    }

    fn atomic_write(&self, value: &Value) -> io::Result<()> {
        let destination = self.runtime_dir.join("lifecycle-status.json");
        let temporary = self.runtime_dir.join(format!(
            ".lifecycle-status.{}.{}.tmp",
            std::process::id(),
            self.next_temporary.fetch_add(1, Ordering::Relaxed)
        ));
        let result =
            write_temporary(&temporary, value).and_then(|()| fs::rename(&temporary, &destination));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> PathBuf {
        self.runtime_dir.join("lifecycle-status.json")
    }
}

fn is_stale_name(name: &str) -> bool {
    matches!(
        name,
        "lifecycle-status.json"
            | "koreader-ready"
            | "koreader-exit"
            | "magicpaper-ready"
            | "magicpaper-exit"
            | "riddle-ready"
            | "riddle-exit"
    ) || name.starts_with(".lifecycle-status.")
        || name.starts_with(".koreader-ready.")
}

fn remove_if_present(path: PathBuf) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn status_value(envelope: &LifecycleEventEnvelope) -> Result<Value, BridgeError> {
    let token = &envelope.body.token;
    let mut status = Map::new();
    status.insert("schema".into(), Value::from(1));
    status.insert("app_id".into(), Value::from(token.app_id.to_string()));
    status.insert("generation".into(), Value::from(token.generation));
    status.insert(
        "foreground_epoch".into(),
        Value::from(token.foreground_epoch),
    );
    status.insert(
        "lease_id".into(),
        token.lease_id.map(Value::from).unwrap_or(Value::Null),
    );
    status.insert(
        "request_id".into(),
        Value::from(envelope.request_id.clone()),
    );
    insert_event_fields(&mut status, &envelope.body.event)?;
    Ok(Value::Object(status))
}

fn insert_event_fields(
    status: &mut Map<String, Value>,
    event: &LifecycleEvent,
) -> Result<(), BridgeError> {
    match event {
        LifecycleEvent::Ready {
            first_frame_sequence,
        } => {
            status.insert("event".into(), Value::from("ready"));
            insert_optional(
                status,
                "first_frame_sequence",
                first_frame_sequence.map(Value::from),
            );
        }
        LifecycleEvent::BackgroundReady {
            title,
            subtitle,
            resume_payload,
        } => {
            status.insert("event".into(), Value::from("background_ready"));
            status.insert("title".into(), Value::from(title.clone()));
            status.insert("subtitle".into(), Value::from(subtitle.clone()));
            insert_optional(status, "resume_payload", resume_payload.clone());
        }
        LifecycleEvent::StateSaved { resume_payload } => {
            status.insert("event".into(), Value::from("state_saved"));
            insert_optional(status, "resume_payload", resume_payload.clone());
        }
        LifecycleEvent::ShutdownComplete { exit_code } => {
            status.insert("event".into(), Value::from("shutdown_complete"));
            status.insert("exit_code".into(), Value::from(*exit_code));
        }
        LifecycleEvent::Failed {
            stage,
            message,
            retryable,
        } => {
            status.insert("event".into(), Value::from("failed"));
            status.insert("stage".into(), serde_json::to_value(stage)?);
            status.insert("message".into(), Value::from(message.clone()));
            status.insert("retryable".into(), Value::from(*retryable));
        }
        LifecycleEvent::Notification { .. } => unreachable!(),
    }
    Ok(())
}

fn insert_optional(status: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        status.insert(key.into(), value);
    }
}

fn write_temporary(path: &std::path::Path, value: &Value) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.flush()
}
