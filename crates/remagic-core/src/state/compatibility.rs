use super::model::*;

/// Converts a persisted/in-flight v1 state into a fenced v2 snapshot. Sentinel
/// generation/epoch/lease values are local compatibility values only; the v2
/// supervisor allocates fresh values before sending any new protocol message.
impl From<ManagerState> for SupervisorState {
    fn from(legacy: ManagerState) -> Self {
        let mut state = SupervisorState {
            state_revision: legacy.sequence,
            last_app: legacy.last_app.clone(),
            ..SupervisorState::default()
        };
        let app_state = match &legacy.domain {
            DomainState::Launching(app) => Some((app.clone(), AppInstanceState::Starting)),
            DomainState::Foreground(app) => Some((app.clone(), AppInstanceState::Foreground)),
            DomainState::Parking(app) => Some((app.clone(), AppInstanceState::Stopping)),
            _ => None,
        };
        state.sleeping = matches!(&legacy.domain, DomainState::Sleeping);
        state.domain = match legacy.domain {
            DomainState::System => SystemDomainState::Stock,
            DomainState::EnteringManaged => SystemDomainState::EnteringManaged,
            DomainState::RestoringSystem => SystemDomainState::LeavingManaged,
            DomainState::Recovering => SystemDomainState::Recovering,
            _ => SystemDomainState::Managed,
        };
        if let Some((app_id, instance_state)) = app_state {
            let foreground = instance_state == AppInstanceState::Foreground;
            let token = AppToken {
                app_id: app_id.clone(),
                generation: 1,
                foreground_epoch: foreground as u64,
                lease_id: foreground.then_some(1),
            };
            state.apps.insert(
                app_id.clone(),
                AppInstance {
                    token,
                    state: instance_state,
                    pid: None,
                    title: String::new(),
                    subtitle: String::new(),
                    last_error: None,
                },
            );
            if foreground {
                state.foreground_app = Some(app_id);
            }
        }
        state
    }
}

impl From<&SupervisorState> for ManagerState {
    fn from(state: &SupervisorState) -> Self {
        let domain = match state.domain {
            SystemDomainState::Stock => DomainState::System,
            SystemDomainState::EnteringManaged => DomainState::EnteringManaged,
            SystemDomainState::LeavingManaged => DomainState::RestoringSystem,
            SystemDomainState::Recovering => DomainState::Recovering,
            SystemDomainState::Managed if state.sleeping => DomainState::Sleeping,
            SystemDomainState::Managed => {
                let active = state
                    .foreground_app
                    .as_ref()
                    .and_then(|id| state.apps.get(id))
                    .or_else(|| {
                        state.apps.values().find(|instance| {
                            matches!(
                                instance.state,
                                AppInstanceState::Starting | AppInstanceState::Stopping
                            )
                        })
                    });
                match active {
                    Some(instance) if instance.state == AppInstanceState::Starting => {
                        DomainState::Launching(instance.token.app_id.clone())
                    }
                    Some(instance) if instance.state == AppInstanceState::Foreground => {
                        DomainState::Foreground(instance.token.app_id.clone())
                    }
                    Some(instance) if instance.state == AppInstanceState::Stopping => {
                        DomainState::Parking(instance.token.app_id.clone())
                    }
                    _ => DomainState::Manager,
                }
            }
        };
        Self {
            domain,
            last_app: state.last_app.clone(),
            sequence: state.state_revision,
        }
    }
}
