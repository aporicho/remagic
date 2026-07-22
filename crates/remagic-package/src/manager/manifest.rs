use super::PackageError;
use remagic_core::{AppId, AppManifest};

pub(crate) fn materialize_manifest_bytes(
    bundled: &[u8],
    app_id: &AppId,
    content_id: &str,
) -> Result<Vec<u8>, PackageError> {
    if content_id.len() != 64
        || !content_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PackageError::State("invalid package content id".into()));
    }
    let text =
        std::str::from_utf8(bundled).map_err(|error| PackageError::Manifest(error.to_string()))?;
    let current = format!("/home/root/apps/{app_id}/current");
    let release = format!("/home/root/apps/{app_id}/releases/{content_id}");
    let materialized = text
        .replace(&format!("{current}/"), &format!("{release}/"))
        .replace(&format!("{current}\""), &format!("{release}\""));
    if materialized.contains(&format!("{current}/"))
        || materialized.contains(&format!("{current}\""))
    {
        return Err(PackageError::Manifest(
            "application manifest retains a mutable current path".into(),
        ));
    }
    let manifest: AppManifest =
        toml::from_str(&materialized).map_err(|error| PackageError::Manifest(error.to_string()))?;
    manifest
        .validate()
        .map_err(|error| PackageError::Manifest(error.to_string()))?;
    if manifest.id != *app_id {
        return Err(PackageError::Manifest(
            "materialized manifest identity changed".into(),
        ));
    }
    Ok(materialized.into_bytes())
}
