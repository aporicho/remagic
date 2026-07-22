use remagic_core::AppManifest;
use remagic_protocol::InputMode;

pub(super) const DIRECT_INK_CAPABILITY: &str = "ink:direct-v1";
pub(super) const DYNAMIC_INPUT_MODE_CAPABILITY: &str = "input:mode-v2";

pub(super) fn supports_dynamic_input_mode(manifest: &AppManifest) -> bool {
    has_capability(manifest, DYNAMIC_INPUT_MODE_CAPABILITY)
}

pub(super) fn initial_input_mode(manifest: &AppManifest) -> InputMode {
    if supports_dynamic_input_mode(manifest) {
        InputMode::AnimationLocked
    } else if has_capability(manifest, DIRECT_INK_CAPABILITY) {
        // Compatibility for applications which predate the negotiated mode
        // protocol. Their manifest-level direct-ink contract remains intact.
        InputMode::Writing
    } else {
        InputMode::AnimationLocked
    }
}

pub(super) fn has_capability(manifest: &AppManifest, expected: &str) -> bool {
    manifest
        .capabilities
        .iter()
        .any(|capability| capability.as_str() == expected)
}
