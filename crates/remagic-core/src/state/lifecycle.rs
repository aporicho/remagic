use super::model::*;
use crate::AppId;

impl SupervisorState {
    pub fn start_app(
        &mut self,
        app_id: AppId,
        generation: u64,
        pid: Option<u32>,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        if self.domain != SystemDomainState::Managed || self.sleeping {
            return Err(StateModelError::LaunchOutsideActiveManaged);
        }
        if generation == 0 {
            return Err(StateModelError::ZeroGeneration(app_id));
        }
        if let Some(previous) = self.apps.get(&app_id) {
            if generation <= previous.token.generation {
                return Err(StateModelError::StaleGeneration {
                    app_id,
                    expected_after: previous.token.generation,
                    actual: generation,
                });
            }
            if !previous.state.is_terminal() {
                return Err(StateModelError::AppAlreadyRunning(app_id));
            }
        }
        let token = AppToken {
            app_id: app_id.clone(),
            generation,
            foreground_epoch: 0,
            lease_id: None,
        };
        self.apps.insert(
            app_id,
            AppInstance {
                token: token.clone(),
                state: AppInstanceState::Starting,
                pid,
                title: String::new(),
                subtitle: String::new(),
                last_error: None,
            },
        );
        self.bump_revision()?;
        Ok(token)
    }

    pub fn grant_foreground(
        &mut self,
        app_id: &AppId,
        generation: u64,
        foreground_epoch: u64,
        lease_id: u64,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        if self.domain != SystemDomainState::Managed || self.sleeping {
            return Err(StateModelError::LaunchOutsideActiveManaged);
        }
        if self.foreground_app.is_some() {
            return Err(StateModelError::MultipleForegroundApps);
        }
        let instance = self.current_instance_mut(app_id, generation)?;
        if !matches!(
            instance.state,
            AppInstanceState::Starting | AppInstanceState::Background
        ) {
            return Err(StateModelError::InvalidAppTransition {
                app_id: app_id.clone(),
                from: instance.state,
                to: AppInstanceState::Foreground,
            });
        }
        if foreground_epoch == 0 || foreground_epoch <= instance.token.foreground_epoch {
            return Err(StateModelError::StaleForegroundEpoch {
                app_id: app_id.clone(),
                expected_after: instance.token.foreground_epoch,
                actual: foreground_epoch,
            });
        }
        if lease_id == 0 {
            return Err(StateModelError::ZeroLease(app_id.clone()));
        }
        instance.token.foreground_epoch = foreground_epoch;
        instance.token.lease_id = Some(lease_id);
        instance.state = AppInstanceState::Foreground;
        let token = instance.token.clone();
        self.foreground_app = Some(app_id.clone());
        self.last_app = Some(app_id.clone());
        self.bump_revision()?;
        Ok(token)
    }

    pub fn mark_background_ready(
        &mut self,
        app_id: &AppId,
        generation: u64,
        title: String,
        subtitle: String,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        let instance = self.current_instance_mut(app_id, generation)?;
        if instance.state != AppInstanceState::Starting {
            return Err(StateModelError::InvalidAppTransition {
                app_id: app_id.clone(),
                from: instance.state,
                to: AppInstanceState::Background,
            });
        }
        instance.state = AppInstanceState::Background;
        instance.title = title;
        instance.subtitle = subtitle;
        let token = instance.token.clone();
        self.bump_revision()?;
        Ok(token)
    }

    pub fn enter_background(
        &mut self,
        app_id: &AppId,
        generation: u64,
        foreground_epoch: u64,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        let instance = self.current_instance_mut(app_id, generation)?;
        if instance.state != AppInstanceState::Foreground
            || instance.token.foreground_epoch != foreground_epoch
        {
            return Err(StateModelError::StaleForegroundEpoch {
                app_id: app_id.clone(),
                expected_after: instance.token.foreground_epoch.saturating_sub(1),
                actual: foreground_epoch,
            });
        }
        instance.state = AppInstanceState::Background;
        instance.token.lease_id = None;
        let token = instance.token.clone();
        self.foreground_app = None;
        self.last_app = Some(app_id.clone());
        self.bump_revision()?;
        Ok(token)
    }

    pub fn begin_stop(
        &mut self,
        app_id: &AppId,
        generation: u64,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        let instance = self.current_instance_mut(app_id, generation)?;
        if instance.state.is_terminal() || instance.state == AppInstanceState::Stopping {
            return Err(StateModelError::InvalidAppTransition {
                app_id: app_id.clone(),
                from: instance.state,
                to: AppInstanceState::Stopping,
            });
        }
        instance.state = AppInstanceState::Stopping;
        instance.token.lease_id = None;
        let token = instance.token.clone();
        if self.foreground_app.as_ref() == Some(app_id) {
            self.foreground_app = None;
        }
        self.bump_revision()?;
        Ok(token)
    }

    pub fn finish_app(
        &mut self,
        app_id: &AppId,
        generation: u64,
        crashed: bool,
        error: Option<String>,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        let instance = self.current_instance_mut(app_id, generation)?;
        if instance.state.is_terminal() {
            return Err(StateModelError::InvalidAppTransition {
                app_id: app_id.clone(),
                from: instance.state,
                to: if crashed {
                    AppInstanceState::Crashed
                } else {
                    AppInstanceState::Exited
                },
            });
        }
        instance.state = if crashed {
            AppInstanceState::Crashed
        } else {
            AppInstanceState::Exited
        };
        instance.token.lease_id = None;
        instance.pid = None;
        instance.last_error = error;
        let token = instance.token.clone();
        if self.foreground_app.as_ref() == Some(app_id) {
            self.foreground_app = None;
        }
        self.bump_revision()?;
        Ok(token)
    }

    pub fn mark_unresponsive(
        &mut self,
        app_id: &AppId,
        generation: u64,
        error: String,
    ) -> Result<AppToken, StateModelError> {
        self.ensure_revision_capacity()?;
        let instance = self.current_instance_mut(app_id, generation)?;
        if instance.state.is_terminal() || instance.state == AppInstanceState::Unresponsive {
            return Err(StateModelError::InvalidAppTransition {
                app_id: app_id.clone(),
                from: instance.state,
                to: AppInstanceState::Unresponsive,
            });
        }
        instance.state = AppInstanceState::Unresponsive;
        instance.token.lease_id = None;
        instance.last_error = Some(error);
        let token = instance.token.clone();
        if self.foreground_app.as_ref() == Some(app_id) {
            self.foreground_app = None;
        }
        self.bump_revision()?;
        Ok(token)
    }
}
