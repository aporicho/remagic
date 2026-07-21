mod compatibility;
mod lifecycle;
mod model;
mod transitions;

pub use model::{
    AppInstance, AppInstanceState, AppToken, DomainState, ManagerState, StateModelError,
    SupervisorState, SystemDomainState, Transition, TransitionError,
};

#[cfg(test)]
mod tests;
