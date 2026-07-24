use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use remagic_device::{DeviceProduct, SUPPORTED_OS_SERIES};

pub const SYSTEM_RELEASE_SCHEMA_V1: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SystemReleaseV1 {
    pub schema: u32,
    pub release_id: String,
    pub version: String,
    pub sequence: u64,
    pub supported_devices: Vec<DeviceProduct>,
    pub supported_os: Vec<String>,
    pub required_remagic_api: u32,
    pub requires_reboot: bool,
    pub archive: SystemArtifactV1,
    pub generated_at_unix: u64,
    pub expires_at_unix: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SystemArtifactV1 {
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SystemReleaseSignatureV1 {
    pub schema: u32,
    pub key_id: String,
    pub algorithm: String,
    pub release_sha256: String,
    pub signature: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SystemTrustedKeyV1 {
    pub key_id: String,
    pub public_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSystemReleaseV1 {
    pub release: SystemReleaseV1,
    pub key_id: String,
    pub digest: String,
}

impl VerifiedSystemReleaseV1 {
    pub fn verify(
        release_bytes: &[u8],
        signature_bytes: &[u8],
        trusted_keys: &[SystemTrustedKeyV1],
        now_unix: u64,
        minimum_sequence: u64,
        device: DeviceProduct,
        os_version: &str,
    ) -> Result<Self, SystemReleaseError> {
        let release: SystemReleaseV1 = serde_json::from_slice(release_bytes)
            .map_err(|error| SystemReleaseError::InvalidRelease(error.to_string()))?;
        release.validate(device, os_version, now_unix, minimum_sequence)?;
        let signature: SystemReleaseSignatureV1 = serde_json::from_slice(signature_bytes)
            .map_err(|error| SystemReleaseError::InvalidSignature(error.to_string()))?;
        if signature.schema != SYSTEM_RELEASE_SCHEMA_V1 || signature.algorithm != "ed25519" {
            return Err(SystemReleaseError::UnsupportedSignature);
        }
        let digest = sha256_hex(release_bytes);
        if signature.release_sha256 != digest {
            return Err(SystemReleaseError::DigestMismatch);
        }
        let key = trusted_keys
            .iter()
            .find(|key| key.key_id == signature.key_id)
            .ok_or_else(|| SystemReleaseError::UntrustedKey(signature.key_id.clone()))?;
        let public_key = STANDARD
            .decode(&key.public_key)
            .map_err(|_| SystemReleaseError::InvalidKey)?;
        let signature_value = STANDARD
            .decode(&signature.signature)
            .map_err(|_| SystemReleaseError::InvalidSignatureEncoding)?;
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(release_bytes, &signature_value)
            .map_err(|_| SystemReleaseError::VerificationFailed)?;
        Ok(Self {
            release,
            key_id: signature.key_id,
            digest,
        })
    }
}

impl SystemReleaseV1 {
    pub fn validate(
        &self,
        device: DeviceProduct,
        os_version: &str,
        now_unix: u64,
        minimum_sequence: u64,
    ) -> Result<(), SystemReleaseError> {
        if self.schema != SYSTEM_RELEASE_SCHEMA_V1
            || self.release_id.trim().is_empty()
            || self.version.trim().is_empty()
            || self.sequence < minimum_sequence
            || self.required_remagic_api == 0
            || self.generated_at_unix >= self.expires_at_unix
            || now_unix < self.generated_at_unix
            || now_unix >= self.expires_at_unix
        {
            return Err(SystemReleaseError::InvalidRelease(
                "metadata or sequence is invalid".into(),
            ));
        }
        if !self.supported_devices.contains(&device)
            || (!self.supported_os.is_empty()
                && !self.supported_os.iter().any(|value| {
                    os_version == value || os_version.starts_with(&format!("{value}."))
                }))
            || !os_version.starts_with(SUPPORTED_OS_SERIES)
        {
            return Err(SystemReleaseError::IncompatibleDevice);
        }
        if !self
            .archive
            .url
            .starts_with("https://github.com/aporicho/remagic/releases/download/")
            || self.archive.size_bytes == 0
            || self.archive.sha256.len() != 64
            || self
                .archive
                .sha256
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit())
        {
            return Err(SystemReleaseError::InvalidArtifact);
        }
        Ok(())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SystemReleaseError {
    #[error("invalid system release: {0}")]
    InvalidRelease(String),
    #[error("invalid release signature: {0}")]
    InvalidSignature(String),
    #[error("unsupported release signature")]
    UnsupportedSignature,
    #[error("release digest does not match signature")]
    DigestMismatch,
    #[error("system release signing key is not trusted: {0}")]
    UntrustedKey(String),
    #[error("system release public key is invalid")]
    InvalidKey,
    #[error("system release signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("system release signature verification failed")]
    VerificationFailed,
    #[error("system release is incompatible with this device")]
    IncompatibleDevice,
    #[error("system release artifact metadata is invalid")]
    InvalidArtifact,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> SystemReleaseV1 {
        SystemReleaseV1 {
            schema: 1,
            release_id: "remagic-test".into(),
            version: "2026.07.23".into(),
            sequence: 2,
            supported_devices: vec![DeviceProduct::PaperProMove],
            supported_os: vec!["3.27.0".into()],
            required_remagic_api: 6,
            requires_reboot: false,
            archive: SystemArtifactV1 {
                url: "https://github.com/aporicho/remagic/releases/download/v2026.07.23/remagic-system-universal-aarch64.tar.gz".into(),
                sha256: "a".repeat(64),
                size_bytes: 1,
            },
            generated_at_unix: 10,
            expires_at_unix: 20,
        }
    }

    #[test]
    fn validates_device_and_sequence() {
        let value = release();
        assert!(value
            .validate(DeviceProduct::PaperProMove, "3.27.0", 15, 2)
            .is_ok());
        assert_eq!(
            value.validate(DeviceProduct::PaperPro, "3.27.0", 15, 2),
            Err(SystemReleaseError::IncompatibleDevice)
        );
        assert!(value
            .validate(DeviceProduct::PaperProMove, "3.27.0", 15, 3)
            .is_err());
        let mut invalid_api = value;
        invalid_api.required_remagic_api = 0;
        assert!(invalid_api
            .validate(DeviceProduct::PaperProMove, "3.27.0", 15, 2)
            .is_err());
    }
}
