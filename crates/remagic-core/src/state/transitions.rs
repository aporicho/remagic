use super::model::*;

impl ManagerState {
    pub fn apply(&mut self, transition: Transition) -> Result<(), TransitionError> {
        use DomainState::*;
        let next = match (&self.domain, transition) {
            (System, Transition::TriplePower) => EnteringManaged,
            (EnteringManaged, Transition::ManagedReady) => Manager,
            (Manager, Transition::Launch(app)) => Launching(app),
            (Manager, Transition::SinglePower) if self.last_app.is_some() => {
                Launching(self.last_app.clone().unwrap())
            }
            (Manager, Transition::TriplePower) => RestoringSystem,
            (Manager, Transition::Sleep) => Sleeping,
            (Sleeping, Transition::Wake) => Manager,
            (Launching(expected), Transition::AppReady(actual)) if expected == &actual => {
                Foreground(actual)
            }
            (Launching(expected), Transition::AppExited(actual)) if expected == &actual => Manager,
            (Launching(expected), Transition::AppCrashed(actual)) if expected == &actual => Manager,
            (Foreground(app), Transition::SinglePower) => Parking(app.clone()),
            (Foreground(app), Transition::TriplePower) => Parking(app.clone()),
            (Foreground(expected), Transition::AppExited(actual)) if expected == &actual => Manager,
            (Foreground(expected), Transition::AppCrashed(actual)) if expected == &actual => {
                Manager
            }
            (Parking(expected), Transition::AppParked(actual)) if expected == &actual => Manager,
            (Parking(expected), Transition::AppRestored(actual)) if expected == &actual => {
                Foreground(actual)
            }
            (Parking(expected), Transition::AppExited(actual)) if expected == &actual => Manager,
            (Parking(expected), Transition::AppCrashed(actual)) if expected == &actual => Manager,
            (RestoringSystem, Transition::SystemReady) => System,
            (_, Transition::Failure) => Recovering,
            (Recovering, Transition::SystemReady) => System,
            (current, transition) => {
                return Err(TransitionError {
                    current: current.clone(),
                    transition,
                })
            }
        };

        match &next {
            Foreground(app) | Parking(app) => self.last_app = Some(app.clone()),
            _ => {}
        }
        self.domain = next;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }
}

impl SupervisorState {
    pub fn transition_domain(&mut self, next: SystemDomainState) -> Result<(), StateModelError> {
        self.ensure_revision_capacity()?;
        use SystemDomainState::*;
        let valid = matches!(
            (self.domain, next),
            (Stock, EnteringManaged)
                | (EnteringManaged, Managed)
                | (Managed, LeavingManaged)
                | (LeavingManaged, Stock)
                | (Recovering, Stock)
                | (Stock, Recovering)
                | (EnteringManaged, Recovering)
                | (Managed, Recovering)
                | (LeavingManaged, Recovering)
        );
        if !valid {
            return Err(StateModelError::InvalidDomainTransition {
                from: self.domain,
                to: next,
            });
        }
        if next == Stock && self.apps.values().any(|app| !app.state.is_terminal()) {
            return Err(StateModelError::RunningAppInStockDomain);
        }
        if next != Managed {
            self.sleeping = false;
            self.foreground_app = None;
            for instance in self.apps.values_mut() {
                instance.token.lease_id = None;
                if instance.state == AppInstanceState::Foreground {
                    instance.state = AppInstanceState::Stopping;
                }
            }
        }
        self.domain = next;
        self.bump_revision()
    }

    pub fn set_sleeping(&mut self, sleeping: bool) -> Result<(), StateModelError> {
        self.ensure_revision_capacity()?;
        if self.domain != SystemDomainState::Managed {
            return Err(StateModelError::SleepOutsideManaged);
        }
        if sleeping && self.foreground_app.is_some() {
            return Err(StateModelError::ForegroundWhileSleeping);
        }
        self.sleeping = sleeping;
        self.bump_revision()
    }
}
