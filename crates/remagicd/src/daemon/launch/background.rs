use super::*;

impl Daemon {
    pub(super) async fn ensure_background_service(
        &self,
        manifest: &remagic_core::AppManifest,
    ) -> Result<(), String> {
        match manifest.effective_background_service() {
            Some(BackgroundService::Systemd { unit }) => {
                if !self.controller.is_active_checked(&unit).await? {
                    self.controller.start_and_wait(&unit).await?
                }
            }
            Some(BackgroundService::Managed {
                exec,
                args,
                working_dir,
                restart,
            }) => {
                let environment = managed_background_environment(manifest)?;
                self.controller
                    .start_managed_background(
                        &manifest.id,
                        &exec,
                        &args,
                        &working_dir,
                        restart,
                        &environment,
                    )
                    .await?;
            }
            None => {}
        }
        Ok(())
    }

    pub(in crate::daemon) async fn start_declared_background_services(&self) {
        let manifests = self
            .manifests
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for manifest in manifests {
            if let Err(error) = self.ensure_background_service(&manifest).await {
                warn!(app_id = %manifest.id, %error, "background service did not start");
            }
        }
    }

    pub(super) async fn start_background_service(
        &self,
        context: &LaunchContext,
    ) -> Result<(), String> {
        self.ensure_background_service(&context.manifest).await
    }
}

fn managed_background_environment(
    manifest: &remagic_core::AppManifest,
) -> Result<Vec<(String, String)>, String> {
    let directories = manifest
        .runtime
        .directories
        .as_ref()
        .ok_or_else(|| format!("application {} has no runtime directories", manifest.id))?;
    let mut values = manifest.environment.clone();
    for (key, value) in [
        ("HOME", directories.home.display().to_string()),
        (
            "XDG_CONFIG_HOME",
            directories.config_home.display().to_string(),
        ),
        ("XDG_DATA_HOME", directories.data_home.display().to_string()),
        (
            "XDG_STATE_HOME",
            directories.state_home.display().to_string(),
        ),
        (
            "XDG_CACHE_HOME",
            directories.cache_home.display().to_string(),
        ),
        ("LANG", manifest.runtime.locale.lang.clone()),
        ("TZ", manifest.runtime.timezone.name.clone()),
        ("REMAGIC_APP_ID", manifest.id.to_string()),
        ("REMAGIC_MANAGED", "1".into()),
        ("REMAGIC_BACKGROUND_SERVICE", "1".into()),
    ] {
        values.insert(key.into(), value);
    }
    Ok(values.into_iter().collect())
}

pub(super) fn should_quiesce_background(
    active: bool,
    manifest: &remagic_core::AppManifest,
) -> bool {
    !active && manifest.data_schema.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cold_schema_launch_quiesces_its_background_writer() {
        let mut manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../../manifests/magicpaper.toml")).unwrap();
        assert!(should_quiesce_background(false, &manifest));
        assert!(!should_quiesce_background(true, &manifest));
        manifest.data_schema = None;
        assert!(!should_quiesce_background(false, &manifest));
    }
}
