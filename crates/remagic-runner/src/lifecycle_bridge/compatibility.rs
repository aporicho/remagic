use super::BridgeError;
use remagic_core::{AppId, AppToken};
use remagic_protocol::{
    Envelope, LifecycleEvent, LifecycleEventBody, LifecycleEventEnvelope, LifecycleStage,
};
use serde_json::{Map, Value};

pub(super) fn decode_event(payload: &[u8]) -> Result<LifecycleEventEnvelope, BridgeError> {
    if let Ok(envelope) = serde_json::from_slice::<LifecycleEventEnvelope>(payload) {
        envelope.validate_header()?;
        envelope.body.validate()?;
        return Ok(envelope);
    }
    let envelope: Envelope<Value> = serde_json::from_slice(payload)?;
    envelope.validate_header()?;
    decode_compatibility_event(envelope)
}

fn decode_compatibility_event(
    envelope: Envelope<Value>,
) -> Result<LifecycleEventEnvelope, BridgeError> {
    let mut fields = envelope
        .body
        .as_object()
        .cloned()
        .ok_or(BridgeError::InvalidEnvelope("event body is not an object"))?;
    let token = compatibility_token(&mut fields)?;
    let kind = take_string(&mut fields, "event")?;
    let event = compatibility_event(&kind, &mut fields)?;
    let body = LifecycleEventBody { token, event };
    body.validate()?;
    Ok(Envelope {
        protocol: envelope.protocol,
        request_id: envelope.request_id,
        expected_state_revision: envelope.expected_state_revision,
        body,
    })
}

fn compatibility_event(
    kind: &str,
    fields: &mut Map<String, Value>,
) -> Result<LifecycleEvent, BridgeError> {
    match kind {
        "ready" | "foreground_ready" => Ok(LifecycleEvent::Ready {
            first_frame_sequence: take_optional_u64(fields, "first_frame_sequence")?,
        }),
        "background_ready" => Ok(LifecycleEvent::BackgroundReady {
            title: take_optional_string(fields, "title").unwrap_or_default(),
            subtitle: take_optional_string(fields, "subtitle").unwrap_or_default(),
            resume_payload: take_payload(fields),
        }),
        "state_saved" => Ok(LifecycleEvent::StateSaved {
            resume_payload: take_payload(fields),
        }),
        "shutdown_complete" => Ok(LifecycleEvent::ShutdownComplete {
            exit_code: take_optional_i32(fields, "exit_code")?.unwrap_or(0),
        }),
        "failed" => Ok(decode_failure(fields)),
        "notification" => Ok(LifecycleEvent::Notification {
            title: take_optional_string(fields, "title").unwrap_or_default(),
            body: take_optional_string(fields, "body").unwrap_or_default(),
        }),
        _ => Err(BridgeError::UnknownEvent(kind.to_owned())),
    }
}

fn decode_failure(fields: &mut Map<String, Value>) -> LifecycleEvent {
    let operation = take_optional_string(fields, "operation");
    let stage = take_optional_string(fields, "stage")
        .as_deref()
        .and_then(parse_stage)
        .or_else(|| operation.as_deref().and_then(stage_for_operation))
        .unwrap_or(LifecycleStage::Runtime);
    let message = take_optional_string(fields, "message")
        .or_else(|| take_optional_string(fields, "reason"))
        .unwrap_or_else(|| "application reported an unspecified failure".into());
    LifecycleEvent::Failed {
        stage,
        message,
        retryable: fields
            .remove("retryable")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    }
}

fn compatibility_token(fields: &mut Map<String, Value>) -> Result<AppToken, BridgeError> {
    let app_id = AppId::new(take_string(fields, "app_id")?)
        .map_err(|_| BridgeError::InvalidEnvelope("invalid compatibility app id"))?;
    Ok(AppToken {
        app_id,
        generation: take_u64(fields, "generation")?,
        foreground_epoch: take_optional_u64(fields, "foreground_epoch")?.unwrap_or(0),
        lease_id: take_optional_u64(fields, "lease_id")?,
    })
}

fn take_payload(fields: &mut Map<String, Value>) -> Option<Value> {
    fields
        .remove("resume_payload")
        .filter(|value| !value.is_null())
}

fn take_string(fields: &mut Map<String, Value>, key: &'static str) -> Result<String, BridgeError> {
    take_optional_string(fields, key).ok_or(BridgeError::MissingField(key))
}

fn take_optional_string(fields: &mut Map<String, Value>, key: &str) -> Option<String> {
    fields
        .remove(key)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
}

fn take_u64(fields: &mut Map<String, Value>, key: &'static str) -> Result<u64, BridgeError> {
    take_optional_u64(fields, key)?.ok_or(BridgeError::MissingField(key))
}

fn take_optional_u64(
    fields: &mut Map<String, Value>,
    key: &'static str,
) -> Result<Option<u64>, BridgeError> {
    match fields.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or(BridgeError::InvalidField(key)),
        Some(Value::String(value)) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| BridgeError::InvalidField(key)),
        Some(_) => Err(BridgeError::InvalidField(key)),
    }
}

fn take_optional_i32(
    fields: &mut Map<String, Value>,
    key: &'static str,
) -> Result<Option<i32>, BridgeError> {
    match fields.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(Some)
            .ok_or(BridgeError::InvalidField(key)),
        Some(_) => Err(BridgeError::InvalidField(key)),
    }
}

fn parse_stage(value: &str) -> Option<LifecycleStage> {
    match value {
        "start" => Some(LifecycleStage::Start),
        "foreground" => Some(LifecycleStage::Foreground),
        "background" => Some(LifecycleStage::Background),
        "save" => Some(LifecycleStage::Save),
        "shutdown" => Some(LifecycleStage::Shutdown),
        "runtime" => Some(LifecycleStage::Runtime),
        _ => None,
    }
}

fn stage_for_operation(value: &str) -> Option<LifecycleStage> {
    match value {
        "start" | "open_path" => Some(LifecycleStage::Start),
        "enter_foreground" => Some(LifecycleStage::Foreground),
        "enter_background" => Some(LifecycleStage::Background),
        "save" => Some(LifecycleStage::Save),
        "shutdown" => Some(LifecycleStage::Shutdown),
        _ => None,
    }
}
