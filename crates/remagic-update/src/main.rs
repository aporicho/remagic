use remagic_core::{
    system_release::{SystemTrustedKeyV1, VerifiedSystemReleaseV1},
    DeviceProduct,
};
use serde::Deserialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const TRUSTED_KEYS: &[u8] =
    include_bytes!("../../../scripts/system-release/system-trusted-keys.json");
const WORK_ROOT: &str = "/home/root/.cache/remagic-update";

#[derive(Deserialize)]
struct TrustedKeyDocument {
    keys: Vec<SystemTrustedKeyV1>,
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("verify") => {
            let release = read_arg(&mut args, "RELEASE_JSON")?;
            let signature = read_arg(&mut args, "SIGNATURE_JSON")?;
            let device = parse_device(&read_arg(&mut args, "DEVICE")?)?;
            let os = read_arg(&mut args, "OS_VERSION")?;
            let minimum = read_arg(&mut args, "MIN_SEQUENCE")?
                .parse()
                .map_err(|_| "invalid minimum sequence".to_owned())?;
            let verified = verify_files(
                &PathBuf::from(release),
                &PathBuf::from(signature),
                device,
                &os,
                minimum,
            )?;
            println!("{}", serde_json::to_string_pretty(&verified.release).map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("check") => {
            let release_url = read_arg(&mut args, "RELEASE_URL")?;
            let signature_url = read_arg(&mut args, "SIGNATURE_URL")?;
            let device = parse_device(&read_arg(&mut args, "DEVICE")?)?;
            let os = read_arg(&mut args, "OS_VERSION")?;
            let minimum = read_arg(&mut args, "MIN_SEQUENCE")?
                .parse()
                .map_err(|_| "invalid minimum sequence".to_owned())?;
            let work = WorkDirectory::create()?;
            let release = work.path().join("release.json");
            let signature = work.path().join("release.sig.json");
            download(&release_url, &release)?;
            download(&signature_url, &signature)?;
            let verified = verify_files(&release, &signature, device, &os, minimum)?;
            println!("{}", serde_json::to_string_pretty(&verified.release).map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("install") => {
            let release_url = read_arg(&mut args, "RELEASE_URL")?;
            let signature_url = read_arg(&mut args, "SIGNATURE_URL")?;
            let device = parse_device(&read_arg(&mut args, "DEVICE")?)?;
            let os = read_arg(&mut args, "OS_VERSION")?;
            let minimum = read_arg(&mut args, "MIN_SEQUENCE")?
                .parse()
                .map_err(|_| "invalid minimum sequence".to_owned())?;
            let work = WorkDirectory::create()?;
            let release = work.path().join("release.json");
            let signature = work.path().join("release.sig.json");
            let archive = work.path().join("remagic-system.tar.gz");
            download(&release_url, &release)?;
            download(&signature_url, &signature)?;
            let verified = verify_files(&release, &signature, device, &os, minimum)?;
            download(&verified.release.archive.url, &archive)?;
            verify_archive(&archive, &verified.release.archive)?;
            apply_archive(&archive, work.path())
        }
        Some("apply") => {
            let release = PathBuf::from(read_arg(&mut args, "RELEASE_JSON")?);
            let signature = PathBuf::from(read_arg(&mut args, "SIGNATURE_JSON")?);
            let archive = PathBuf::from(read_arg(&mut args, "ARCHIVE")?);
            let device = parse_device(&read_arg(&mut args, "DEVICE")?)?;
            let os = read_arg(&mut args, "OS_VERSION")?;
            let minimum = read_arg(&mut args, "MIN_SEQUENCE")?
                .parse()
                .map_err(|_| "invalid minimum sequence".to_owned())?;
            let verified = verify_files(&release, &signature, device, &os, minimum)?;
            verify_archive(&archive, &verified.release.archive)?;
            let work = WorkDirectory::create()?;
            apply_archive(&archive, work.path())
        }
        _ => Err("usage: remagic-update verify RELEASE_JSON SIGNATURE_JSON DEVICE OS MIN_SEQUENCE | check RELEASE_URL SIGNATURE_URL DEVICE OS MIN_SEQUENCE | install RELEASE_URL SIGNATURE_URL DEVICE OS MIN_SEQUENCE | apply RELEASE_JSON SIGNATURE_JSON ARCHIVE DEVICE OS MIN_SEQUENCE".into()),
    }
}

struct WorkDirectory {
    path: PathBuf,
}

impl WorkDirectory {
    fn create() -> Result<Self, String> {
        Self::create_at(Path::new(WORK_ROOT))
    }

    fn create_at(base: &Path) -> Result<Self, String> {
        fs::create_dir_all(base).map_err(|e| e.to_string())?;
        fs::set_permissions(base, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        cleanup_stale_work_directories(base)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = base.join(format!("work-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cleanup_stale_work_directories(base: &Path) -> Result<(), String> {
    for entry in fs::read_dir(base).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(pid) = name
            .strip_prefix("work-")
            .and_then(|tail| tail.split('-').next())
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if !Path::new("/proc").join(pid.to_string()).exists() {
            fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn verify_files(
    release: &PathBuf,
    signature: &PathBuf,
    device: DeviceProduct,
    os: &str,
    minimum: u64,
) -> Result<VerifiedSystemReleaseV1, String> {
    let keys = serde_json::from_slice::<TrustedKeyDocument>(TRUSTED_KEYS)
        .map_err(|e| e.to_string())?
        .keys;
    VerifiedSystemReleaseV1::verify(
        &fs::read(release).map_err(|e| e.to_string())?,
        &fs::read(signature).map_err(|e| e.to_string())?,
        &keys,
        unix_now(),
        minimum,
        device,
        os,
    )
    .map_err(|e| e.to_string())
}

fn download(url: &str, output: &PathBuf) -> Result<(), String> {
    if !url.starts_with("https://github.com/aporicho/remagic/releases/") {
        return Err("update URL is outside the trusted ReMagic GitHub release path".into());
    }
    let curl = PathBuf::from("/home/root/apps/remagic/bin/curl");
    let mut command = if curl.is_file() {
        let mut command = Command::new(curl);
        command.args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--connect-timeout",
            "15",
            "--max-time",
            "60",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--output",
        ]);
        command
    } else {
        let mut command = Command::new("/usr/bin/wget");
        command.args(["-q", "-T", "60", "-t", "3", "-O"]);
        command
    };
    let status = command
        .arg(output)
        .arg(url)
        .status()
        .map_err(|e| e.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("download failed: {status}"))
}

fn verify_archive(
    archive: &PathBuf,
    artifact: &remagic_core::system_release::SystemArtifactV1,
) -> Result<(), String> {
    let size = fs::metadata(archive).map_err(|e| e.to_string())?.len();
    if size != artifact.size_bytes {
        return Err("system archive size does not match signed metadata".into());
    }
    let output = Command::new("sha256sum")
        .arg(archive)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("cannot calculate system archive SHA-256".into());
    }
    let digest = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    (digest == artifact.sha256)
        .then_some(())
        .ok_or_else(|| "system archive SHA-256 does not match signed metadata".into())
}

fn apply_archive(archive: &PathBuf, work: &Path) -> Result<(), String> {
    let root = work.join("system");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(archive)
        .arg("-C")
        .arg(&root)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cannot extract system archive: {status}"));
    }
    let installer = root.join("remagic-system/install-device.sh");
    if !installer.is_file() {
        return Err("system archive has no installer".into());
    }
    let status = Command::new(&installer)
        .current_dir(root.join("remagic-system"))
        .status()
        .map_err(|e| e.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("system installation failed: {status}"))
}

fn parse_device(value: &str) -> Result<DeviceProduct, String> {
    match value {
        "paper_pro" | "ferrari" => Ok(DeviceProduct::PaperPro),
        "paper_pro_move" | "chiappa" => Ok(DeviceProduct::PaperProMove),
        _ => Err(format!("unsupported device: {value}")),
    }
}

fn read_arg(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("missing {name}"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_directory_is_private_and_removed_on_drop() {
        let base = std::env::temp_dir().join(format!(
            "remagic-update-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = {
            let work = WorkDirectory::create_at(&base).unwrap();
            fs::write(work.path().join("payload"), b"fixture").unwrap();
            assert_eq!(
                fs::metadata(work.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            work.path().to_owned()
        };
        assert!(!path.exists());
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn stale_work_directory_is_removed_before_a_new_transaction() {
        let base = std::env::temp_dir().join(format!(
            "remagic-update-stale-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(base.join("work-4294967295-dead")).unwrap();
        let work = WorkDirectory::create_at(&base).unwrap();
        assert!(!base.join("work-4294967295-dead").exists());
        drop(work);
        fs::remove_dir(base).unwrap();
    }
}
