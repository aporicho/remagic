use super::*;
use std::io::Read;

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
                let agent_generation = manifest
                    .capabilities
                    .iter()
                    .any(|capability| capability.as_str() == "agent:pi-v1")
                    .then(|| self.allocate_generation());
                let environment = managed_background_environment(manifest, agent_generation)?;
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
    agent_generation: Option<u64>,
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
    if manifest
        .capabilities
        .iter()
        .any(|capability| capability.as_str() == "agent:pi-v1")
    {
        let generation = agent_generation.ok_or_else(|| {
            format!(
                "application {} has no background agent generation",
                manifest.id
            )
        })?;
        let token = background_agent_token()?;
        for (key, value) in [
            (
                "REMAGIC_AGENT_SOCKET",
                remagic_protocol::DEFAULT_AGENT_SOCKET.into(),
            ),
            ("REMAGIC_AGENT_TOKEN", token),
            ("REMAGIC_AGENT_PRINCIPAL", "background".into()),
            ("REMAGIC_APP_GENERATION", generation.to_string()),
        ] {
            values.insert(key.into(), value);
        }
    }
    Ok(values.into_iter().collect())
}

fn background_agent_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| format!("cannot create background agent token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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

    #[test]
    fn managed_agent_background_receives_an_independent_private_identity() {
        let manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../../manifests/magicpaper.toml")).unwrap();
        let environment = managed_background_environment(&manifest, Some(37))
            .unwrap()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment["REMAGIC_AGENT_SOCKET"],
            remagic_protocol::DEFAULT_AGENT_SOCKET
        );
        assert_eq!(environment["REMAGIC_AGENT_PRINCIPAL"], "background");
        assert_eq!(environment["REMAGIC_AGENT_TOKEN"].len(), 64);
        assert_eq!(environment["REMAGIC_APP_GENERATION"], "37");
    }

    #[test]
    fn managed_agent_background_requires_a_manager_generation() {
        let manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../../manifests/magicpaper.toml")).unwrap();
        assert!(managed_background_environment(&manifest, None).is_err());
    }
}
