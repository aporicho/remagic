use std::time::{Duration, Instant};

const WAKE_GUARD_QUIET: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WakeGesture {
    pub power_key: bool,
}

#[derive(Debug, Default)]
pub(super) struct WakeGuard {
    pub(super) active: bool,
    resumed_at: Option<Instant>,
    last_event_at: Option<Instant>,
    key_down: bool,
    power_key_after_resume: bool,
    resume_reply: Option<tokio::sync::oneshot::Sender<Result<WakeGesture, String>>>,
}

impl WakeGuard {
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let resumed_at = self.resumed_at?;
        if self.key_down {
            return None;
        }
        Some(
            self.last_event_at
                .filter(|event| *event > resumed_at)
                .unwrap_or(resumed_at)
                + WAKE_GUARD_QUIET,
        )
    }

    pub(super) fn is_armed_before_resume(&self) -> bool {
        self.active && self.resumed_at.is_none()
    }

    pub(super) fn arm(&mut self) -> Result<(), String> {
        if self.active {
            return Err("power wake guard is already armed".into());
        }
        self.active = true;
        self.resumed_at = None;
        self.last_event_at = None;
        self.key_down = false;
        Ok(())
    }

    pub(super) fn resume(&mut self, now: Instant) -> Result<(), String> {
        if !self.active {
            return Err("power wake guard is not armed".into());
        }
        if self.resumed_at.is_some() {
            return Err("power wake guard was already resumed".into());
        }
        self.resumed_at = Some(now);
        Ok(())
    }

    pub(super) fn resume_and_report(
        &mut self,
        now: Instant,
        reply: tokio::sync::oneshot::Sender<Result<WakeGesture, String>>,
    ) {
        match self.resume(now) {
            Ok(()) => self.resume_reply = Some(reply),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// Returns true while this raw event belongs to the suspend/wake fence.
    pub(super) fn consume(&mut self, value: i32, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        if self.resumed_at.is_some() && matches!(value, 0 | 1) {
            self.power_key_after_resume = true;
        }
        match value {
            1 => self.key_down = true,
            0 => self.key_down = false,
            _ => return true,
        }
        self.last_event_at = Some(now);
        true
    }

    pub(super) fn poll(&mut self, now: Instant) {
        let Some(resumed_at) = self.resumed_at else {
            return;
        };
        if self.key_down {
            return;
        }
        let quiet_since = self
            .last_event_at
            .filter(|event| *event > resumed_at)
            .unwrap_or(resumed_at);
        if now.duration_since(quiet_since) >= WAKE_GUARD_QUIET {
            self.finish();
        }
    }

    fn finish(&mut self) {
        let gesture = WakeGesture {
            power_key: self.power_key_after_resume,
        };
        let reply = self.resume_reply.take();
        *self = Self::default();
        if let Some(reply) = reply {
            let _ = reply.send(Ok(gesture));
        }
    }

    pub(super) fn cancel(&mut self) {
        let reply = self.resume_reply.take();
        *self = Self::default();
        if let Some(reply) = reply {
            let _ = reply.send(Err("power wake guard was cancelled".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_press_and_release_are_consumed_until_the_quiet_window() {
        let t = Instant::now();
        let mut guard = WakeGuard::default();
        guard.arm().unwrap();
        assert!(guard.consume(1, t));
        guard.resume(t + Duration::from_millis(20)).unwrap();
        assert!(guard.consume(0, t + Duration::from_millis(80)));
        guard.poll(t + Duration::from_millis(879));
        assert!(guard.active);
        guard.poll(t + Duration::from_millis(880));
        assert!(!guard.active);
    }

    #[test]
    fn non_power_wake_is_released_after_a_quiet_window() {
        let t = Instant::now();
        let mut guard = WakeGuard::default();
        guard.arm().unwrap();
        guard.resume(t).unwrap();
        guard.poll(t + WAKE_GUARD_QUIET);
        assert!(!guard.active);
        assert!(!guard.consume(1, t + WAKE_GUARD_QUIET));
    }

    #[test]
    fn non_power_wake_reports_no_power_key_gesture() {
        let t = Instant::now();
        let mut guard = WakeGuard::default();
        let (reply, mut result) = tokio::sync::oneshot::channel();
        guard.arm().unwrap();
        guard.resume_and_report(t, reply);
        guard.poll(t + WAKE_GUARD_QUIET);

        assert_eq!(
            result.try_recv().unwrap().unwrap(),
            WakeGesture { power_key: false }
        );
    }

    #[test]
    fn power_wake_reports_power_key_gesture() {
        let t = Instant::now();
        let mut guard = WakeGuard::default();
        let (reply, mut result) = tokio::sync::oneshot::channel();
        guard.arm().unwrap();
        guard.resume_and_report(t, reply);
        assert!(guard.consume(1, t + Duration::from_millis(10)));
        assert!(guard.consume(0, t + Duration::from_millis(40)));
        guard.poll(t + Duration::from_millis(840));

        assert_eq!(
            result.try_recv().unwrap().unwrap(),
            WakeGesture { power_key: true }
        );
    }

    #[test]
    fn held_wake_key_keeps_the_guard_armed() {
        let t = Instant::now();
        let mut guard = WakeGuard::default();
        guard.arm().unwrap();
        assert!(guard.consume(1, t));
        guard.resume(t + Duration::from_millis(10)).unwrap();
        guard.poll(t + Duration::from_secs(5));
        assert!(guard.active);
        assert!(guard.consume(0, t + Duration::from_secs(5)));
        guard.poll(t + Duration::from_secs(5) + WAKE_GUARD_QUIET);
        assert!(!guard.active);
    }

    #[test]
    fn duplicate_arm_and_resume_are_rejected() {
        let t = Instant::now();
        let mut guard = WakeGuard::default();
        guard.arm().unwrap();
        assert!(guard.arm().is_err());
        guard.resume(t).unwrap();
        assert!(guard.resume(t).is_err());
    }
}
