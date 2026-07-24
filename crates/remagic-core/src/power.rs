use std::time::{Duration, Instant};

pub const MULTI_CLICK_GAP: Duration = Duration::from_millis(800);
pub const LONG_PRESS: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerAction {
    None,
    Single,
    Triple,
    Long,
}

#[derive(Debug, Default)]
pub struct ClickTracker {
    clicks: u8,
    deadline: Option<Instant>,
    down_since: Option<Instant>,
    suppress_release: bool,
}

impl ClickTracker {
    pub fn next_deadline(&self) -> Option<Instant> {
        let held = self.down_since.map(|start| start + LONG_PRESS);
        match (held, self.deadline) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        }
    }

    pub fn press(&mut self, now: Instant) {
        self.down_since.get_or_insert(now);
    }

    pub fn release(&mut self, now: Instant) -> PowerAction {
        if self.suppress_release {
            self.suppress_release = false;
            self.down_since = None;
            return PowerAction::None;
        }
        let Some(start) = self.down_since.take() else {
            // EVIOCGRAB changes, suspend/resume and input-device reopen can
            // expose a release without the corresponding press.  It is not a
            // user click and must never recall an application.
            return PowerAction::None;
        };
        if now.duration_since(start) >= LONG_PRESS {
            self.clear();
            return PowerAction::Long;
        }
        if self.deadline.is_some_and(|deadline| now > deadline) {
            self.clicks = 0;
        }
        self.clicks = self.clicks.saturating_add(1);
        if self.clicks >= 3 {
            self.clear();
            PowerAction::Triple
        } else {
            self.deadline = Some(now + MULTI_CLICK_GAP);
            PowerAction::None
        }
    }

    pub fn poll(&mut self, now: Instant) -> PowerAction {
        if self
            .down_since
            .is_some_and(|start| now.duration_since(start) >= LONG_PRESS)
        {
            self.clicks = 0;
            self.deadline = None;
            self.down_since = None;
            // poll() fires while the key is still physically held.  Consume
            // the matching release instead of turning it into a delayed
            // single click 800 ms later.
            self.suppress_release = true;
            return PowerAction::Long;
        }
        if self.clicks > 0 && self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            PowerAction::Single
        } else {
            PowerAction::None
        }
    }

    pub fn clear(&mut self) {
        self.clicks = 0;
        self.deadline = None;
        self.down_since = None;
        self.suppress_release = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(tracker: &mut ClickTracker, at: Instant) -> PowerAction {
        tracker.press(at);
        tracker.release(at + Duration::from_millis(60))
    }

    #[test]
    fn triple_fires_on_third_release() {
        let t = Instant::now();
        let mut tracker = ClickTracker::default();
        assert_eq!(click(&mut tracker, t), PowerAction::None);
        assert_eq!(
            click(&mut tracker, t + Duration::from_millis(200)),
            PowerAction::None
        );
        assert_eq!(
            click(&mut tracker, t + Duration::from_millis(400)),
            PowerAction::Triple
        );
    }

    #[test]
    fn single_resolves_after_gap() {
        let t = Instant::now();
        let mut tracker = ClickTracker::default();
        click(&mut tracker, t);
        assert_eq!(
            tracker.poll(t + Duration::from_millis(859)),
            PowerAction::None
        );
        assert_eq!(
            tracker.poll(t + Duration::from_millis(861)),
            PowerAction::Single
        );
    }

    #[test]
    fn long_press_does_not_become_single() {
        let t = Instant::now();
        let mut tracker = ClickTracker::default();
        tracker.press(t);
        assert_eq!(
            tracker.release(t + Duration::from_secs(4)),
            PowerAction::Long
        );
        assert_eq!(tracker.poll(t + Duration::from_secs(5)), PowerAction::None);
    }

    #[test]
    fn polled_long_press_consumes_the_later_release() {
        let t = Instant::now();
        let mut tracker = ClickTracker::default();
        tracker.press(t);
        assert_eq!(tracker.poll(t + LONG_PRESS), PowerAction::Long);
        assert_eq!(
            tracker.release(t + LONG_PRESS + Duration::from_millis(50)),
            PowerAction::None
        );
        assert_eq!(
            tracker.poll(t + LONG_PRESS + MULTI_CLICK_GAP),
            PowerAction::None
        );
    }

    #[test]
    fn orphan_release_is_not_a_click() {
        let t = Instant::now();
        let mut tracker = ClickTracker::default();
        assert_eq!(tracker.release(t), PowerAction::None);
        assert_eq!(tracker.poll(t + MULTI_CLICK_GAP), PowerAction::None);
    }

    #[test]
    fn clear_removes_a_polled_long_press_release_suppression() {
        let t = Instant::now();
        let mut tracker = ClickTracker::default();
        tracker.press(t);
        assert_eq!(tracker.poll(t + LONG_PRESS), PowerAction::Long);
        tracker.clear();
        assert_eq!(tracker.release(t + LONG_PRESS), PowerAction::None);

        let next = t + LONG_PRESS + Duration::from_secs(1);
        assert_eq!(click(&mut tracker, next), PowerAction::None);
        assert_eq!(
            tracker.poll(next + Duration::from_millis(861)),
            PowerAction::Single
        );
    }
}
