use super::*;
use remagic_core::{AppManifest, RuntimeDirectories};
use remagic_protocol::{Response, SyncAction};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const SYNC_CAPABILITY: &str = "sync:koreader-state-v1";
const EXCHANGE_DIR: &str = "sync-exchange";

impl Daemon {
    pub(super) async fn sync_request(
        &self,
        requester: AppId,
        provider: AppId,
        action: SyncAction,
    ) -> Response {
        match self.sync_request_inner(&requester, &provider, action).await {
            Ok(output) => Response::SyncOutput {
                success: true,
                output,
            },
            Err(message) => Response::SyncOutput {
                success: false,
                output: message,
            },
        }
    }

    async fn sync_request_inner(
        &self,
        requester_id: &AppId,
        provider_id: &AppId,
        action: SyncAction,
    ) -> Result<String, String> {
        let manifests = self.manifests.read().await;
        let requester = manifests
            .get(requester_id)
            .cloned()
            .ok_or_else(|| format!("unknown sync requester {requester_id}"))?;
        let provider = manifests
            .get(provider_id)
            .cloned()
            .ok_or_else(|| format!("unknown sync provider {provider_id}"))?;
        drop(manifests);
        if !requester
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == SYNC_CAPABILITY)
        {
            return Err(format!(
                "application {requester_id} lacks {SYNC_CAPABILITY}"
            ));
        }
        let provider_contract = provider
            .sync_provider
            .clone()
            .ok_or_else(|| format!("application {provider_id} is not a sync provider"))?;
        let directories = requester
            .runtime
            .directories
            .as_ref()
            .ok_or_else(|| "sync requester has no runtime directories".to_owned())?;
        let exchange = prepare_exchange_root(directories).map_err(|error| error.to_string())?;

        match action {
            SyncAction::Prepare => {
                let response = self.enqueue(Event::Close(provider_id.clone(), true)).await;
                if let Response::Error { message } = response {
                    return Err(message);
                }
                if self
                    .controller
                    .is_active(&utils::app_unit(provider_id))
                    .await
                {
                    return Err(format!("application {provider_id} did not stop"));
                }
                Ok("provider stopped and data is stable".into())
            }
            SyncAction::Export { output } => {
                ensure_provider_stopped(self, provider_id).await?;
                let output = validate_exchange_output(&exchange, &output)?;
                run_provider(
                    &provider,
                    &provider_contract.exporter,
                    "export",
                    &output,
                    provider_contract.timeout_ms,
                )
                .await
            }
            SyncAction::Import { input } => {
                ensure_provider_stopped(self, provider_id).await?;
                let input = validate_exchange_input(&exchange, &input)?;
                run_provider(
                    &provider,
                    &provider_contract.importer,
                    "import",
                    &input,
                    provider_contract.timeout_ms,
                )
                .await
            }
            SyncAction::Finish => Ok("sync session finished".into()),
        }
    }
}

async fn ensure_provider_stopped(daemon: &Daemon, provider: &AppId) -> Result<(), String> {
    if daemon
        .controller
        .is_active(&utils::app_unit(provider))
        .await
    {
        Err(format!(
            "application {provider} must be prepared before data exchange"
        ))
    } else {
        Ok(())
    }
}

fn prepare_exchange_root(directories: &RuntimeDirectories) -> std::io::Result<PathBuf> {
    let root = directories.data_home.join(EXCHANGE_DIR);
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    root.canonicalize()
}

fn safe_leaf(path: &Path) -> bool {
    path.file_name().is_some()
        && path
            .components()
            .all(|part| !matches!(part, Component::ParentDir | Component::CurDir))
}

fn validate_exchange_output(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || !safe_leaf(path) || path.parent() != Some(root) {
        return Err("sync export path is outside the requester exchange directory".into());
    }
    if fs::symlink_metadata(path).is_ok() {
        return Err("sync export path already exists".into());
    }
    Ok(path.to_path_buf())
}

fn validate_exchange_input(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || !safe_leaf(path) || path.parent() != Some(root) {
        return Err("sync import path is outside the requester exchange directory".into());
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 * 1024
    {
        return Err("sync import must be a bounded regular file".into());
    }
    Ok(path.to_path_buf())
}

async fn run_provider(
    manifest: &AppManifest,
    executable: &Path,
    operation: &str,
    exchange_file: &Path,
    timeout_ms: u64,
) -> Result<String, String> {
    if !executable.is_file() {
        return Err(format!(
            "sync provider executable is missing: {}",
            executable.display()
        ));
    }
    let directories = manifest
        .runtime
        .directories
        .as_ref()
        .ok_or_else(|| "sync provider has no runtime directories".to_owned())?;
    let mut command = Command::new(executable);
    command
        .arg(operation)
        .arg(exchange_file)
        .current_dir(&manifest.working_dir)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", &directories.home)
        .env("XDG_CONFIG_HOME", &directories.config_home)
        .env("XDG_DATA_HOME", &directories.data_home)
        .env("XDG_STATE_HOME", &directories.state_home)
        .env("XDG_CACHE_HOME", &directories.cache_home)
        .envs(&manifest.environment);
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), command.output())
        .await
        .map_err(|_| "sync provider timed out".to_owned())?
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return Err(if stderr.is_empty() {
            format!("sync provider exited with {}", output.status)
        } else {
            stderr
        });
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_paths_are_single_regular_files_beneath_the_owned_root() {
        let root = std::env::temp_dir().join(format!("remagic-sync-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let output = root.join("state.json");
        assert_eq!(validate_exchange_output(&root, &output).unwrap(), output);
        assert!(validate_exchange_output(&root, &root.join("../escape")).is_err());
        fs::write(&output, b"{}").unwrap();
        assert_eq!(validate_exchange_input(&root, &output).unwrap(), output);
        fs::remove_dir_all(root).unwrap();
    }
}
