use remagic_core::{AppId, DomainState};

#[derive(Debug, Eq, PartialEq)]
pub(super) enum LaunchRoute {
    AlreadyForeground,
    Park(AppId),
    Manager,
}

pub(super) fn launch_route(
    domain: &DomainState,
    id: &AppId,
    no_path: bool,
) -> Result<LaunchRoute, String> {
    match domain {
        DomainState::Foreground(current) if current == id && no_path => {
            Ok(LaunchRoute::AlreadyForeground)
        }
        DomainState::Foreground(current) => Ok(LaunchRoute::Park(current.clone())),
        DomainState::Manager => Ok(LaunchRoute::Manager),
        _ => Err("applications can only be launched from manager or foreground app".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_of_current_app_without_path_is_idempotent() {
        let app = AppId::new("magicpaper").unwrap();
        assert_eq!(
            launch_route(&DomainState::Foreground(app.clone()), &app, true).unwrap(),
            LaunchRoute::AlreadyForeground,
        );
        assert_eq!(
            launch_route(&DomainState::Foreground(app.clone()), &app, false).unwrap(),
            LaunchRoute::Park(app),
        );
    }
}
