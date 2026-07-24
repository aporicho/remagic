use remagic_core::PowerSettings;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(super) fn load(path: &Path) -> Result<PowerSettings, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PowerSettings::default())
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let settings: PowerSettings = toml::from_str(&text)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    settings.validate()?;
    Ok(settings)
}

pub(super) fn save(path: &Path, settings: &PowerSettings) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("power settings path {} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(".power.toml.tmp-{}", std::process::id()));
    let encoded = toml::to_string_pretty(settings)
        .map_err(|error| format!("cannot encode power settings: {error}"))?;
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.write_all(encoded.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot commit {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync {}: {error}", parent.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
