//! Versioned application-to-manager request protocol.
//!
//! This newline-delimited JSON channel is intentionally separate from the
//! length-prefixed manager control protocol. Applications use it for narrowly
//! scoped foreground requests such as opening another app or changing their
//! direct-ink input mode.

use remagic_core::{AppId, AppToken, ResourceLease, WorkClass};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const RUNTIME_APP_PROTOCOL_V1: u8 = 1;
pub const RUNTIME_APP_PROTOCOL_V2: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Writing,
    AnimationLocked,
    Modal,
}

impl InputMode {
    pub fn ink_enabled(self) -> bool {
        matches!(self, Self::Writing)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAppRequest {
    #[serde(default)]
    pub version: u8,
    #[serde(default)]
    pub request_id: String,
    #[serde(flatten)]
    pub command: RuntimeAppCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum RuntimeAppCommand {
    OpenApp {
        app: AppId,
        #[serde(default)]
        open_path: Option<PathBuf>,
        /// Version two binds the handoff to the caller's exact foreground
        /// lease. It remains optional on the wire so version-one clients keep
        /// their original request shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<AppToken>,
    },
    SetInputMode {
        /// Exact process and foreground-lease identity received over the
        /// lifecycle channel. The manager never infers this from an app id:
        /// doing so would let a delayed request mutate a newer foreground.
        token: AppToken,
        mode: InputMode,
    },
    BeginWork {
        class: WorkClass,
        reason: String,
        requested_ms: u64,
    },
    FinishWork {
        lease_id: u64,
        #[serde(default)]
        visible_result: bool,
    },
}

/// A single response shape keeps the legacy `open_app` acknowledgement byte
/// compatible while allowing `set_input_mode` to return its applied state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAppReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<InputMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<AppToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<ResourceLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RuntimeAppReply {
    pub fn legacy_open_accepted() -> Self {
        Self {
            ok: true,
            status: Some("accepted".into()),
            request_id: None,
            mode: None,
            token: None,
            ink_enabled: None,
            lease: None,
            error: None,
        }
    }

    pub fn open_accepted(request_id: String) -> Self {
        Self {
            request_id: Some(request_id),
            ..Self::legacy_open_accepted()
        }
    }

    pub fn input_mode_accepted(
        request_id: String,
        token: AppToken,
        mode: InputMode,
        ink_enabled: bool,
    ) -> Self {
        Self {
            ok: true,
            status: Some("accepted".into()),
            request_id: Some(request_id),
            mode: Some(mode),
            token: Some(token),
            ink_enabled: Some(ink_enabled),
            lease: None,
            error: None,
        }
    }

    pub fn work_accepted(request_id: String, lease: ResourceLease) -> Self {
        Self {
            ok: true,
            status: Some("accepted".into()),
            request_id: Some(request_id),
            mode: None,
            token: None,
            ink_enabled: None,
            lease: Some(lease),
            error: None,
        }
    }

    pub fn work_finished(request_id: String) -> Self {
        Self {
            ok: true,
            status: Some("finished".into()),
            request_id: Some(request_id),
            mode: None,
            token: None,
            ink_enabled: None,
            lease: None,
            error: None,
        }
    }

    pub fn error(request_id: Option<String>, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            status: None,
            request_id,
            mode: None,
            token: None,
            ink_enabled: None,
            lease: None,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_open_app_request_and_ack_remain_wire_compatible() {
        let request: RuntimeAppRequest = serde_json::from_str(
            r#"{"version":1,"request_id":"legacy-1","command":"open_app","app":"koreader","open_path":"/home/root/book.epub"}"#,
        )
        .unwrap();
        assert!(matches!(
            request.command,
            RuntimeAppCommand::OpenApp {
                app,
                open_path: Some(path),
                token: None,
            } if app.as_str() == "koreader" && path == std::path::Path::new("/home/root/book.epub")
        ));
        assert_eq!(
            serde_json::to_value(RuntimeAppReply::legacy_open_accepted()).unwrap(),
            serde_json::json!({"ok": true, "status": "accepted"})
        );
        assert_eq!(
            serde_json::to_string(&RuntimeAppReply::legacy_open_accepted()).unwrap(),
            r#"{"ok":true,"status":"accepted"}"#
        );
    }

    #[test]
    fn version_two_open_app_carries_exact_foreground_token_and_correlates_ack() {
        let request: RuntimeAppRequest = serde_json::from_str(
            r#"{"version":2,"request_id":"open-42","command":"open_app","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"app":"koreader","open_path":"/home/root/book.epub"}"#,
        )
        .unwrap();
        assert!(matches!(
            request.command,
            RuntimeAppCommand::OpenApp {
                app,
                open_path: Some(path),
                token: Some(AppToken {
                    app_id,
                    generation: 7,
                    foreground_epoch: 11,
                    lease_id: Some(13),
                }),
            } if app.as_str() == "koreader"
                && app_id.as_str() == "magicpaper"
                && path == std::path::Path::new("/home/root/book.epub")
        ));
        assert_eq!(
            serde_json::to_value(RuntimeAppReply::open_accepted("open-42".into())).unwrap(),
            serde_json::json!({
                "ok": true,
                "status": "accepted",
                "request_id": "open-42",
            })
        );
    }

    #[test]
    fn every_input_mode_round_trips_and_maps_direct_ink() {
        for (wire, mode, enabled) in [
            ("writing", InputMode::Writing, true),
            ("animation_locked", InputMode::AnimationLocked, false),
            ("modal", InputMode::Modal, false),
        ] {
            let encoded = format!(
                r#"{{"version":2,"request_id":"mode-1","command":"set_input_mode","token":{{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13}},"mode":"{wire}"}}"#
            );
            let request: RuntimeAppRequest = serde_json::from_str(&encoded).unwrap();
            assert!(matches!(
                request.command,
                RuntimeAppCommand::SetInputMode { ref token, mode: actual }
                    if token.app_id.as_str() == "magicpaper"
                        && token.generation == 7
                        && token.foreground_epoch == 11
                        && token.lease_id == Some(13)
                        && actual == mode
            ));
            assert_eq!(mode.ink_enabled(), enabled);
        }
    }

    #[test]
    fn input_mode_ack_has_the_exact_correlated_shape() {
        let reply = RuntimeAppReply::input_mode_accepted(
            "mode-42".into(),
            AppToken {
                app_id: AppId::new("magicpaper").unwrap(),
                generation: 7,
                foreground_epoch: 11,
                lease_id: Some(13),
            },
            InputMode::AnimationLocked,
            false,
        );
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            serde_json::json!({
                "ok": true,
                "status": "accepted",
                "request_id": "mode-42",
                "mode": "animation_locked",
                "token": {
                    "app_id": "magicpaper",
                    "generation": 7,
                    "foreground_epoch": 11,
                    "lease_id": 13
                },
                "ink_enabled": false,
            })
        );
        assert_eq!(
            serde_json::to_string(&reply).unwrap(),
            r#"{"ok":true,"status":"accepted","request_id":"mode-42","mode":"animation_locked","token":{"app_id":"magicpaper","generation":7,"foreground_epoch":11,"lease_id":13},"ink_enabled":false}"#
        );
    }

    #[test]
    fn correlated_errors_keep_the_request_identity() {
        assert_eq!(
            serde_json::to_value(RuntimeAppReply::error(
                Some("mode-9".into()),
                "not foreground",
            ))
            .unwrap(),
            serde_json::json!({
                "ok": false,
                "request_id": "mode-9",
                "error": "not foreground",
            })
        );
    }

    #[test]
    fn finite_work_lease_commands_are_versioned_and_bounded_by_the_manager() {
        let begin: RuntimeAppRequest = serde_json::from_str(
            r#"{"version":2,"request_id":"work-1","command":"begin_work","class":"agent_turn","reason":"scheduled task","requested_ms":180000}"#,
        )
        .unwrap();
        assert!(matches!(
            begin.command,
            RuntimeAppCommand::BeginWork {
                class: WorkClass::AgentTurn,
                requested_ms: 180_000,
                ..
            }
        ));
        let finish: RuntimeAppRequest = serde_json::from_str(
            r#"{"version":2,"request_id":"work-2","command":"finish_work","lease_id":7,"visible_result":false}"#,
        )
        .unwrap();
        assert!(matches!(
            finish.command,
            RuntimeAppCommand::FinishWork {
                lease_id: 7,
                visible_result: false,
            }
        ));
    }
}
