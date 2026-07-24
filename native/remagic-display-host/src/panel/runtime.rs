use super::render::LivePenPoint;
use super::{
    PanelBackend, PanelCommand, PanelLease, PanelTelemetry, RefreshIntent, SubmissionReason,
};
use crate::geometry::Rect;
use crate::surface::SharedSurface;
use std::collections::HashMap;
use std::io;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod presentation;

pub(super) const LIVE_SWAP_INTERVAL: Duration = Duration::from_millis(8);
pub(super) const CANONICAL_SETTLE_DELAY: Duration = Duration::from_millis(280);
pub(super) const CANONICAL_SETTLE_RETRY: Duration = Duration::from_millis(40);
pub(super) const CANONICAL_SETTLE_LIMIT: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InkLease {
    pub(super) key: i32,
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) enabled: bool,
    pub(super) region: Option<Rect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PanelLock {
    pub(super) sleep_epoch: u64,
    pub(super) lease: PanelLease,
    /// Sequence copied into the host-owned framebuffer when the lock was
    /// committed. Refreshing after resume must describe these frozen pixels,
    /// not a newer client commit which was never copied to the panel buffer.
    pub(super) frozen_surface_sequence: u64,
}

pub struct PanelRuntime<B: PanelBackend> {
    pub(super) backend: B,
    receiver: Receiver<PanelCommand>,
    pub(super) surfaces: HashMap<i32, Arc<SharedSurface>>,
    pub(super) foreground: Option<PanelLease>,
    pub(super) lock: Option<PanelLock>,
    pub(super) ink: InkLease,
    pub(super) active_pen: bool,
    pub(super) last_pen: Option<LivePenPoint>,
    pub(super) live_dirty: Rect,
    pub(super) canonical_dirty: Rect,
    pub(super) last_live_submit: Instant,
    pub(super) settle_deadline: Option<Instant>,
    pub(super) settle_started: Option<Instant>,
    pub(super) ink_begin_sequence: u64,
    telemetry: Arc<PanelTelemetry>,
}

impl<B: PanelBackend> PanelRuntime<B> {
    pub fn new(backend: B, receiver: Receiver<PanelCommand>) -> Self {
        Self::with_telemetry(backend, receiver, Arc::new(PanelTelemetry::default()))
    }

    pub fn with_telemetry(
        backend: B,
        receiver: Receiver<PanelCommand>,
        telemetry: Arc<PanelTelemetry>,
    ) -> Self {
        Self {
            backend,
            receiver,
            surfaces: HashMap::new(),
            foreground: None,
            lock: None,
            ink: InkLease::default(),
            active_pen: false,
            last_pen: None,
            live_dirty: Rect::default(),
            canonical_dirty: Rect::default(),
            last_live_submit: Instant::now() - LIVE_SWAP_INTERVAL,
            settle_deadline: None,
            settle_started: None,
            ink_begin_sequence: 0,
            telemetry,
        }
    }

    pub fn run(mut self) -> io::Result<()> {
        loop {
            let timeout = self.next_timeout();
            match self.receiver.recv_timeout(timeout) {
                Ok(command) => {
                    self.telemetry.command_dequeued();
                    if matches!(command, PanelCommand::Shutdown) {
                        break;
                    }
                    if let Err(error) = self.handle(command) {
                        if is_stale_control_error(&error) {
                            eprintln!("remagic-display-host: ignored stale panel command: {error}");
                        } else {
                            return Err(error);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.flush_deferred_damage()?;
            self.flush_deadlines()?;
            self.backend.process_events();
        }
        Ok(())
    }

    fn next_timeout(&self) -> Duration {
        let now = Instant::now();
        // Commands wake the channel immediately. This timeout exists only to
        // service vendor Qt events while the panel is otherwise idle.
        let mut timeout = Duration::from_millis(500);
        if !self.live_dirty.is_empty() {
            timeout = timeout
                .min((self.last_live_submit + LIVE_SWAP_INTERVAL).saturating_duration_since(now));
        }
        if let Some(deadline) = self.settle_deadline {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }
        timeout
    }

    fn handle(&mut self, command: PanelCommand) -> io::Result<()> {
        match command {
            PanelCommand::RegisterSurface(surface) => {
                self.surfaces.insert(surface.key, surface);
            }
            PanelCommand::DropSurface { key } => self.drop_surface(key),
            PanelCommand::Damage {
                lease,
                rect,
                intent,
            } => self.handle_damage(lease, rect, intent)?,
            PanelCommand::SetForeground {
                lease,
                full_refresh,
            } => self.set_foreground(lease, full_refresh)?,
            PanelCommand::ActivateForeground {
                lease,
                ink_enabled,
                full_refresh,
            } => self.activate_foreground(lease, ink_enabled, full_refresh)?,
            PanelCommand::ClearForeground { lease } => self.clear_foreground(lease),
            PanelCommand::ConfigureInk {
                lease,
                enabled,
                region,
            } => self.configure_ink(lease, enabled, region)?,
            PanelCommand::Pen { lease, frame } => self.handle_pen(lease, frame)?,
            PanelCommand::FullRefresh { lease } => self.full_refresh(lease)?,
            PanelCommand::ShowLock { lease, sleep_epoch } => self.show_lock(lease, sleep_epoch)?,
            PanelCommand::RefreshLock { lease, sleep_epoch } => {
                self.refresh_lock(lease, sleep_epoch)?
            }
            PanelCommand::CancelLock {
                lease,
                sleep_epoch,
                replacement_surface_sequence,
            } => self.cancel_lock(lease, sleep_epoch, replacement_surface_sequence)?,
            PanelCommand::Shutdown => unreachable!(),
        }
        Ok(())
    }

    fn drop_surface(&mut self, key: i32) {
        self.surfaces.remove(&key);
        self.telemetry.discard_deferred_damage_for_key(key);
        // A committed lock is a frozen host-owned image. The Home QTFB
        // client may restart while the device sleeps; dropping its writable
        // surface must not remove the lock or its foreground input fence.
        if self.lock.is_some_and(|lock| lock.lease.key == key) {
            return;
        }
        if self
            .foreground
            .is_some_and(|foreground| foreground.key == key)
        {
            self.clear_foreground_unchecked();
        }
    }

    fn handle_damage(
        &mut self,
        lease: PanelLease,
        rect: Rect,
        intent: RefreshIntent,
    ) -> io::Result<()> {
        if self.foreground != Some(lease) {
            return Ok(());
        }
        if (self.active_pen || self.settle_deadline.is_some())
            && self.ink.enabled
            && self.ink.key == lease.key
        {
            self.canonical_dirty = self.canonical_dirty.union(rect);
            Ok(())
        } else {
            self.present_surface(lease, rect, intent, SubmissionReason::SurfaceDamage)
        }
    }

    fn set_foreground(&mut self, lease: PanelLease, full_refresh: bool) -> io::Result<()> {
        let intent = if full_refresh {
            RefreshIntent::Full
        } else {
            RefreshIntent::Content
        };
        self.set_foreground_with(lease, intent, SubmissionReason::ForegroundSwitch)
    }

    fn show_lock(&mut self, lease: PanelLease, sleep_epoch: u64) -> io::Result<()> {
        self.set_foreground_with(lease, RefreshIntent::Full, SubmissionReason::LockScreen)?;
        let frozen_surface_sequence = self
            .telemetry
            .last_presented()
            .filter(|(key, sequence)| *key == lease.key && *sequence > 0)
            .map(|(_, sequence)| sequence)
            .ok_or_else(|| io::Error::other("lock pixels were not recorded as presented"))?;
        self.lock = Some(PanelLock {
            sleep_epoch,
            lease,
            frozen_surface_sequence,
        });
        self.telemetry.commit_lock(sleep_epoch);
        Ok(())
    }

    fn set_foreground_with(
        &mut self,
        lease: PanelLease,
        intent: RefreshIntent,
        reason: SubmissionReason,
    ) -> io::Result<()> {
        let Some(surface) = self.surfaces.get(&lease.key).cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("foreground surface {} is not connected", lease.key),
            ));
        };
        let previous = self.foreground;
        if let Some(previous) = previous.filter(|previous| *previous != lease) {
            self.telemetry.discard_deferred_damage(previous);
        }
        self.abort_ink();
        self.ink = InkLease::default();
        self.foreground = Some(lease);
        if let Err(error) = self.present_surface(lease, surface.full_rect(), intent, reason) {
            self.foreground = previous;
            return Err(error);
        }
        self.telemetry.commit_foreground(lease);
        self.telemetry.commit_ink(lease, false);
        Ok(())
    }

    fn activate_foreground(
        &mut self,
        lease: PanelLease,
        ink_enabled: bool,
        full_refresh: bool,
    ) -> io::Result<()> {
        let Some(surface) = self.surfaces.get(&lease.key).cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("foreground surface {} is not connected", lease.key),
            ));
        };
        let previous = self.foreground;
        if let Some(previous) = previous.filter(|previous| *previous != lease) {
            self.telemetry.discard_deferred_damage(previous);
        }
        self.abort_ink();
        self.foreground = Some(lease);
        self.ink = InkLease {
            key: lease.key,
            generation: lease.generation,
            epoch: lease.foreground_epoch,
            enabled: ink_enabled,
            region: None,
        };
        let intent = if full_refresh {
            RefreshIntent::Full
        } else {
            RefreshIntent::Content
        };
        if let Err(error) = self.present_surface(
            lease,
            surface.full_rect(),
            intent,
            SubmissionReason::ForegroundSwitch,
        ) {
            self.foreground = previous;
            self.ink = InkLease::default();
            return Err(error);
        }
        // Input routing becomes possible only after both the ink policy and
        // the visible pixels belong to this exact lease.
        self.telemetry.commit_ink(lease, ink_enabled);
        self.telemetry.commit_foreground(lease);
        Ok(())
    }

    fn configure_ink(
        &mut self,
        lease: PanelLease,
        enabled: bool,
        region: Option<Rect>,
    ) -> io::Result<()> {
        let valid = self.foreground == Some(lease);
        if enabled && !valid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "direct ink lease does not match the foreground epoch",
            ));
        }
        self.abort_ink();
        self.ink = InkLease {
            key: lease.key,
            generation: lease.generation,
            epoch: lease.foreground_epoch,
            enabled,
            region,
        };
        self.telemetry.commit_ink(lease, enabled);
        Ok(())
    }

    fn full_refresh(&mut self, lease: PanelLease) -> io::Result<()> {
        if self.foreground != Some(lease) {
            return Ok(());
        }
        let Some(surface) = self.surfaces.get(&lease.key) else {
            return Ok(());
        };
        self.present_surface(
            lease,
            surface.full_rect(),
            RefreshIntent::Full,
            SubmissionReason::FullRefresh,
        )
    }

    fn refresh_lock(&mut self, lease: PanelLease, sleep_epoch: u64) -> io::Result<()> {
        let Some(lock) = self.lock else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "lock refresh does not match the committed lock lease",
            ));
        };
        if self.foreground != Some(lease) || lock.sleep_epoch != sleep_epoch || lock.lease != lease
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "lock refresh does not match the committed lock lease",
            ));
        }
        // The panel framebuffer is the frozen, host-owned lock image. Do not
        // copy the client-writable QTFB surface again after resume.
        self.submit(
            Rect::new(0, 0, self.backend.width(), self.backend.height()),
            RefreshIntent::Full,
            lease,
            lock.frozen_surface_sequence,
            SubmissionReason::LockRefresh,
        )?;
        self.telemetry
            .mark_presented(lease.key, lock.frozen_surface_sequence);
        Ok(())
    }

    fn cancel_lock(
        &mut self,
        lease: PanelLease,
        sleep_epoch: u64,
        replacement_surface_sequence: u64,
    ) -> io::Result<()> {
        if !self
            .lock
            .is_some_and(|lock| lock.sleep_epoch == sleep_epoch && lock.lease == lease)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "lock cancellation does not match the committed lock lease",
            ));
        }
        let Some(surface) = self.surfaces.get(&lease.key) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "unlock replacement surface is not connected",
            ));
        };
        if replacement_surface_sequence == 0
            || surface.commit_sequence() < replacement_surface_sequence
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "unlock replacement surface has not reached the required commit",
            ));
        }
        // Keep the lock and input fence in force until the complete manager
        // image is physically submitted. Only then publish the cancellation
        // ACK consumed by HostState and remagicd.
        // Preserve the frozen lock buffer if the backend rejects the unlock
        // submission. A later lock refresh must never expose replacement
        // pixels from a transaction that did not commit.
        let frozen_pixels = self.backend.pixels_mut().to_vec();
        if let Err(error) = self.present_surface(
            lease,
            surface.full_rect(),
            RefreshIntent::Full,
            SubmissionReason::UnlockScreen,
        ) {
            self.backend.pixels_mut().copy_from_slice(&frozen_pixels);
            return Err(error);
        }
        self.lock = None;
        self.telemetry.clear_committed_lock(sleep_epoch);
        // This is the transaction ACK, distinct from committed_lock_epoch=0:
        // zero also describes a ShowLock command that is still queued.
        self.telemetry.record_lock_cancelled(sleep_epoch);
        Ok(())
    }
    fn flush_deferred_damage(&mut self) -> io::Result<()> {
        let Some(lease) = self.foreground else {
            return Ok(());
        };
        let Some(damage) = self.telemetry.take_deferred_damage(lease) else {
            return Ok(());
        };
        self.handle_damage(lease, damage.rect, damage.intent)
    }

    pub(super) fn abort_ink(&mut self) {
        self.active_pen = false;
        self.last_pen = None;
        self.live_dirty = Rect::default();
        self.canonical_dirty = Rect::default();
        self.settle_deadline = None;
        self.settle_started = None;
        self.ink_begin_sequence = 0;
    }

    fn clear_foreground(&mut self, lease: PanelLease) {
        if self.foreground != Some(lease) {
            return;
        }
        self.clear_foreground_unchecked();
    }

    fn clear_foreground_unchecked(&mut self) {
        if let Some(lease) = self.foreground {
            self.telemetry.discard_deferred_damage(lease);
            self.telemetry.clear_committed_foreground(lease);
        }
        self.abort_ink();
        self.foreground = None;
        self.ink = InkLease::default();
    }
}

fn is_stale_control_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::AlreadyExists
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::InvalidInput
    )
}

#[cfg(test)]
mod tests;
