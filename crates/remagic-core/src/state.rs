use crate::AppId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainState {
    System,
    EnteringManaged,
    Manager,
    Launching(AppId),
    Foreground(AppId),
    Parking(AppId),
    RestoringSystem,
    Sleeping,
    Recovering,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transition {
    TriplePower,
    SinglePower,
    Launch(AppId),
    AppReady(AppId),
    AppParked(AppId),
    AppExited(AppId),
    AppCrashed(AppId),
    ManagedReady,
    SystemReady,
    Sleep,
    Wake,
    Failure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagerState {
    pub domain: DomainState,
    pub last_app: Option<AppId>,
    pub sequence: u64,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self {
            domain: DomainState::System,
            last_app: None,
            sequence: 0,
        }
    }
}

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

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("transition {transition:?} is invalid while in {current:?}")]
pub struct TransitionError {
    pub current: DomainState,
    pub transition: Transition,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> AppId {
        AppId::new(value).unwrap()
    }

    #[test]
    fn system_manager_app_round_trip() {
        let mut state = ManagerState::default();
        state.apply(Transition::TriplePower).unwrap();
        state.apply(Transition::ManagedReady).unwrap();
        state.apply(Transition::Launch(id("koreader"))).unwrap();
        state.apply(Transition::AppReady(id("koreader"))).unwrap();
        state.apply(Transition::SinglePower).unwrap();
        state.apply(Transition::AppParked(id("koreader"))).unwrap();
        assert_eq!(state.domain, DomainState::Manager);
        assert_eq!(state.last_app, Some(id("koreader")));
        state.apply(Transition::SinglePower).unwrap();
        assert_eq!(state.domain, DomainState::Launching(id("koreader")));
    }

    #[test]
    fn manager_triple_returns_system() {
        let mut state = ManagerState {
            domain: DomainState::Manager,
            ..ManagerState::default()
        };
        state.apply(Transition::TriplePower).unwrap();
        state.apply(Transition::SystemReady).unwrap();
        assert_eq!(state.domain, DomainState::System);
    }

    #[test]
    fn foreground_crash_returns_to_manager() {
        let app = id("magicpaper");
        let mut state = ManagerState {
            domain: DomainState::Foreground(app.clone()),
            last_app: Some(app.clone()),
            sequence: 3,
        };
        state.apply(Transition::AppCrashed(app.clone())).unwrap();
        assert_eq!(state.domain, DomainState::Manager);
        assert_eq!(state.last_app, Some(app));
    }

    #[test]
    fn foreground_normal_exit_returns_directly_to_manager() {
        let app = id("koreader");
        let mut state = ManagerState {
            domain: DomainState::Foreground(app.clone()),
            last_app: Some(app.clone()),
            sequence: 4,
        };
        state.apply(Transition::AppExited(app.clone())).unwrap();
        assert_eq!(state.domain, DomainState::Manager);
        assert_eq!(state.last_app, Some(app));
    }
}
