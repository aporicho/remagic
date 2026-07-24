use super::*;

impl RuntimeInputState {
    pub(in crate::daemon) fn matches(self, token: &AppToken) -> bool {
        self.generation == token.generation
            && self.foreground_epoch == token.foreground_epoch
            && token.lease_id == Some(self.lease_id)
    }
}

pub(super) fn validate_active_foreground_token(
    domain: &DomainState,
    states: &BTreeMap<AppId, RuntimeInputState>,
    token: &AppToken,
) -> Result<RuntimeInputState, String> {
    token.validate().map_err(|error| error.to_string())?;
    let id = &token.app_id;
    if !matches!(domain, DomainState::Foreground(current) if current == id) {
        return Err(format!(
            "application {id} is not the current foreground application"
        ));
    }
    let state = states
        .get(id)
        .copied()
        .ok_or_else(|| format!("application {id} has no active input fence"))?;
    if !state.matches(token) {
        return Err(format!(
            "application {id} supplied a stale input-mode token"
        ));
    }
    if state.pending {
        return Err(format!("application {id} input fence is still pending"));
    }
    Ok(state)
}

impl Daemon {
    pub(in crate::daemon) async fn validate_runtime_launch_authority(
        &self,
        authority: &RuntimeLaunchAuthority,
    ) -> Result<(), String> {
        match authority {
            RuntimeLaunchAuthority::LegacyPeer(peer) => self.validate_foreground_peer(peer).await,
            RuntimeLaunchAuthority::ForegroundToken(token) => {
                self.validate_foreground_token(token).await
            }
        }
    }

    /// Authorize a runtime request against the exact foreground generation,
    /// epoch, and display lease. The transition lock makes the snapshot
    /// stable with respect to park, close, and app-switch operations.
    pub(in crate::daemon) async fn validate_foreground_token(
        &self,
        token: &AppToken,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        let modes = self.runtime_input_modes.read().await;
        validate_active_foreground_token(&domain, &modes, token).map(|_| ())
    }

    /// Legacy version-one requests have no lease token. They are accepted
    /// only from the cgroup of the app which currently owns the foreground.
    pub(in crate::daemon) async fn validate_foreground_peer(
        &self,
        peer: &AppId,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        if matches!(&domain, DomainState::Foreground(current) if current == peer) {
            Ok(())
        } else {
            Err(format!(
                "application {peer} is not the current foreground application"
            ))
        }
    }
}
