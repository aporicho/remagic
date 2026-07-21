use super::DataSchemaError;
use remagic_core::LaunchEnvironment;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(super) fn run_migrator(
    migrator: &Path,
    working_dir: &Path,
    environment: &LaunchEnvironment,
    from_version: Option<u32>,
    to_version: u32,
    backup_directory: &Path,
    timeout_ms: u64,
) -> Result<(), DataSchemaError> {
    let executable = open_migrator(migrator)?;
    let executable_fd = executable.as_raw_fd();
    let mut command = Command::new(format!("/proc/self/fd/{executable_fd}"));
    command
        .current_dir(working_dir)
        .env_clear()
        .envs(&environment.variables)
        .env(
            "REMAGIC_DATA_SCHEMA_FROM",
            from_version.unwrap_or(0).to_string(),
        )
        .env("REMAGIC_DATA_SCHEMA_TO", to_version.to_string())
        .env("REMAGIC_DATA_SCHEMA_BACKUP", backup_directory)
        .stdin(Stdio::null())
        // A migrator cannot accidentally print API credentials inherited in
        // its application environment into the system journal.
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(executable_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(executable_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|source| DataSchemaError::StartMigrator {
            path: migrator.to_path_buf(),
            source,
        })?;
    drop(executable);
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_process_group(pid);
                return if status.success() {
                    Ok(())
                } else {
                    Err(DataSchemaError::MigratorFailed {
                        path: migrator.to_path_buf(),
                        status: status
                            .code()
                            .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                    })
                };
            }
            Ok(None) if Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Ok(None) => {
                kill_process_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(DataSchemaError::MigratorTimedOut {
                    path: migrator.to_path_buf(),
                    timeout_ms,
                });
            }
            Err(source) => {
                kill_process_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(DataSchemaError::StartMigrator {
                    path: migrator.to_path_buf(),
                    source,
                });
            }
        }
    }
}

fn open_migrator(path: &Path) -> Result<File, DataSchemaError> {
    // The pathname is policy-checked for symbolic ancestors, then opened with
    // O_NOFOLLOW and validated through the resulting descriptor. Execution is
    // through this same descriptor, so a rename after validation cannot swap
    // in different root-privileged code.
    reject_symlink_ancestors(path)?;
    let executable = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| DataSchemaError::InvalidMigrator(path.to_path_buf()))?;
    let metadata = executable
        .metadata()
        .map_err(|_| DataSchemaError::InvalidMigrator(path.to_path_buf()))?;
    let effective_uid = unsafe { libc::geteuid() };
    if !migrator_metadata_is_safe(&metadata, effective_uid) {
        return Err(DataSchemaError::InvalidMigrator(path.to_path_buf()));
    }
    Ok(executable)
}

fn migrator_metadata_is_safe(metadata: &fs::Metadata, effective_uid: u32) -> bool {
    !metadata.file_type().is_symlink()
        && metadata.is_file()
        && metadata.uid() == effective_uid
        && metadata.permissions().mode() & 0o022 == 0
        && metadata.permissions().mode() & 0o111 != 0
        && metadata.mode() & (libc::S_ISUID | libc::S_ISGID) == 0
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), DataSchemaError> {
    let mut ancestors = path.ancestors();
    let _ = ancestors.next();
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|_| DataSchemaError::InvalidMigrator(path.to_path_buf()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DataSchemaError::InvalidMigrator(path.to_path_buf()));
        }
    }
    Ok(())
}

fn kill_process_group(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn migrator_must_be_owned_by_the_effective_uid_and_not_writable_by_others() {
        let path =
            std::env::temp_dir().join(format!("remagic-unsafe-migrator-{}", std::process::id()));
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let effective_uid = unsafe { libc::geteuid() };
        assert!(migrator_metadata_is_safe(&metadata, effective_uid));
        assert!(!migrator_metadata_is_safe(
            &metadata,
            effective_uid.saturating_add(1)
        ));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o722)).unwrap();
        let writable = fs::symlink_metadata(&path).unwrap();
        assert!(!migrator_metadata_is_safe(&writable, effective_uid));
        assert!(matches!(
            open_migrator(&path),
            Err(DataSchemaError::InvalidMigrator(actual)) if actual == path
        ));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn validated_descriptor_executes_original_file_after_path_replacement() {
        let root = std::env::temp_dir().join(format!(
            "remagic-migrator-fd-binding-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("migrate");
        let original = root.join("migrate.original");
        let marker = root.join("marker");
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf original > '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        let executable = open_migrator(&path).unwrap();
        fs::rename(&path, &original).unwrap();
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf replacement > '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        let descriptor = executable.as_raw_fd();
        let mut command = Command::new(format!("/proc/self/fd/{descriptor}"));
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(descriptor, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        assert!(command.status().unwrap().success());
        assert_eq!(fs::read_to_string(marker).unwrap(), "original");
        fs::remove_dir_all(root).unwrap();
    }
}
