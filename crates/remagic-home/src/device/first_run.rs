use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const DEFAULT_MARKER: &str = "/home/root/.local/state/remagic/welcome-v1";

pub(super) fn pending() -> bool {
    !marker_path().is_file()
}

pub(super) fn complete() -> io::Result<()> {
    complete_at(&marker_path())
}

fn marker_path() -> PathBuf {
    std::env::var_os("REMAGIC_WELCOME_MARKER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MARKER))
}

fn complete_at(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid welcome marker"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".welcome-v1.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(b"completed\n")?;
    file.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn completion_is_atomic_and_idempotent() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("remagic-welcome-{id}"));
        let marker = root.join("state/welcome-v1");
        complete_at(&marker).unwrap();
        complete_at(&marker).unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), "completed\n");
        assert_eq!(
            fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }
}
