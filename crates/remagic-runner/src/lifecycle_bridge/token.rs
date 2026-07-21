use super::BridgeError;
use remagic_core::{AppId, AppToken};
use remagic_protocol::LifecycleEventBody;

#[derive(Debug)]
pub(super) struct TokenState {
    pub(super) token: AppToken,
    next_lease_id: u64,
}

impl TokenState {
    pub(super) fn new(
        app_id: AppId,
        generation: u64,
        foreground_epoch: u64,
        lease_id: u64,
    ) -> Self {
        Self {
            token: AppToken {
                app_id,
                generation,
                foreground_epoch,
                lease_id: Some(lease_id),
            },
            next_lease_id: lease_id.wrapping_add(1).max(1),
        }
    }

    pub(super) fn current(&self) -> &AppToken {
        &self.token
    }

    pub(super) fn foreground(&mut self) -> Result<AppToken, BridgeError> {
        self.token.foreground_epoch = self
            .token
            .foreground_epoch
            .checked_add(1)
            .ok_or(BridgeError::TokenExhausted)?;
        let lease_id = self.next_lease_id.max(1);
        self.next_lease_id = self.next_lease_id.wrapping_add(1).max(1);
        self.token.lease_id = Some(lease_id);
        Ok(self.token.clone())
    }

    pub(super) fn foreground_with_fence(
        &mut self,
        foreground_epoch: Option<u64>,
        lease_id: Option<u64>,
    ) -> Result<AppToken, BridgeError> {
        let (Some(foreground_epoch), Some(lease_id)) = (foreground_epoch, lease_id) else {
            return if foreground_epoch.is_none() && lease_id.is_none() {
                self.foreground()
            } else {
                Err(BridgeError::IncompleteForegroundFence)
            };
        };
        if foreground_epoch == 0 || lease_id == 0 {
            return Err(BridgeError::InvalidForegroundFence);
        }
        if foreground_epoch <= self.token.foreground_epoch {
            return Err(BridgeError::StaleForegroundEpoch {
                current: self.token.foreground_epoch,
                requested: foreground_epoch,
            });
        }
        self.token.foreground_epoch = foreground_epoch;
        self.token.lease_id = Some(lease_id);
        self.next_lease_id = lease_id.wrapping_add(1).max(1);
        Ok(self.token.clone())
    }
}

pub(super) fn event_matches_token(event: &LifecycleEventBody, current: &AppToken) -> bool {
    if !event
        .token
        .same_process(&current.app_id, current.generation)
        || event.token.foreground_epoch != current.foreground_epoch
    {
        return false;
    }
    event.token.lease_id == current.lease_id
}

#[cfg(test)]
pub(super) type TokenCursor = TokenState;
