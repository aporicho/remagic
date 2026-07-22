use remagic_core::power::{ClickTracker, PowerAction};
use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tokio_mpsc;

use crate::daemon::{Event, QueuedEvent};

const EV_KEY: u16 = 1;
const KEY_POWER: u16 = 116;
const EVIOCGRAB: libc::c_ulong = 0x40044590;
const WAKE_GUARD_QUIET: Duration = Duration::from_millis(800);

#[derive(Debug)]
pub enum Control {
    Grab {
        grab: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Start consuming the complete key gesture which may wake the device.
    /// This is armed before `/sys/power/state` is written.
    ArmWakeGuard {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Mark the point at which the suspend syscall returned. The guard stays
    /// active until the key is up and the evdev stream has remained quiet.
    ResumeWakeGuard {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    CancelWakeGuard {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Default)]
struct WakeGuard {
    active: bool,
    resumed_at: Option<Instant>,
    last_event_at: Option<Instant>,
    key_down: bool,
}

impl WakeGuard {
    fn arm(&mut self) -> Result<(), String> {
        if self.active {
            return Err("power wake guard is already armed".into());
        }
        self.active = true;
        self.resumed_at = None;
        self.last_event_at = None;
        self.key_down = false;
        Ok(())
    }

    fn resume(&mut self, now: Instant) -> Result<(), String> {
        if !self.active {
            return Err("power wake guard is not armed".into());
        }
        if self.resumed_at.is_some() {
            return Err("power wake guard was already resumed".into());
        }
        self.resumed_at = Some(now);
        Ok(())
    }

    /// Returns true while this raw event belongs to the suspend/wake fence.
    fn consume(&mut self, value: i32, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        match value {
            1 => self.key_down = true,
            0 => self.key_down = false,
            _ => return true,
        }
        self.last_event_at = Some(now);
        true
    }

    fn poll(&mut self, now: Instant) {
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
            self.cancel();
        }
    }

    fn cancel(&mut self) {
        *self = Self::default();
    }
}

pub fn spawn(
    events: tokio_mpsc::Sender<QueuedEvent>,
    launch_interrupt_epoch: Arc<AtomicU64>,
) -> (std::thread::JoinHandle<()>, mpsc::Sender<Control>) {
    let (control_tx, control_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut device = loop {
            match PowerDevice::open() {
                Ok(device) => break device,
                Err(error) => {
                    eprintln!("remagicd: power device unavailable: {error}");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        };
        let mut tracker = ClickTracker::default();
        let mut wake_guard = WakeGuard::default();
        loop {
            while let Ok(control) = control_rx.try_recv() {
                match control {
                    Control::Grab { grab, reply } => {
                        // Domain ownership changes form an input fence. Never
                        // carry a partial click or wake gesture across it.
                        tracker.clear();
                        wake_guard.cancel();
                        let mut result = device.set_grab(grab);
                        if result.is_err() && !grab {
                            // Closing the input fd is the kernel-level escape
                            // hatch for a failed EVIOCGRAB(false).  Reopen it
                            // afterwards so future triple presses still work.
                            result = device.force_release_and_reopen();
                        }
                        let _ = reply.send(result.map_err(|error| error.to_string()));
                    }
                    Control::ArmWakeGuard { reply } => {
                        tracker.clear();
                        let result = if device.grabbed {
                            wake_guard.arm()
                        } else {
                            Err("cannot arm wake guard while the power key is not grabbed".into())
                        };
                        let _ = reply.send(result);
                    }
                    Control::ResumeWakeGuard { reply } => {
                        let _ = reply.send(wake_guard.resume(Instant::now()));
                    }
                    Control::CancelWakeGuard { reply } => {
                        tracker.clear();
                        wake_guard.cancel();
                        let _ = reply.send(Ok(()));
                    }
                }
            }
            for value in device.drain() {
                let now = Instant::now();
                if consume_wake_guard_event(&mut wake_guard, value, now, &launch_interrupt_epoch) {
                    continue;
                }
                let action = if value == 1 {
                    // Invalidate auto-resuspend and cancellable launches on
                    // physical contact, not 800 ms later when a single click
                    // is finally distinguishable from a triple click.
                    note_power_press(&launch_interrupt_epoch);
                    tracker.press(now);
                    PowerAction::None
                } else if value == 0 {
                    tracker.release(now)
                } else {
                    PowerAction::None
                };
                send_action(action, &events, &launch_interrupt_epoch);
            }
            let now = Instant::now();
            wake_guard.poll(now);
            if !wake_guard.active {
                send_action(tracker.poll(now), &events, &launch_interrupt_epoch);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    (thread, control_tx)
}

fn note_power_press(interaction_epoch: &AtomicU64) -> u64 {
    interaction_epoch
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        .wrapping_add(1)
}

fn consume_wake_guard_event(
    wake_guard: &mut WakeGuard,
    value: i32,
    now: Instant,
    interaction_epoch: &AtomicU64,
) -> bool {
    // A press between arming the guard and entering the kernel is user intent,
    // not a wake gesture yet. Record it before consuming the raw event so the
    // suspend transaction's final fence check can yield to the user.
    if wake_guard.active && wake_guard.resumed_at.is_none() && value == 1 {
        note_power_press(interaction_epoch);
    }
    wake_guard.consume(value, now)
}

fn send_action(
    action: PowerAction,
    events: &tokio_mpsc::Sender<QueuedEvent>,
    launch_interrupt_epoch: &AtomicU64,
) {
    let event = match action {
        PowerAction::None => return,
        PowerAction::Single => Event::SinglePower,
        PowerAction::Triple => Event::TriplePower,
        PowerAction::Long => Event::LongPower,
    };
    let _ = events.blocking_send(QueuedEvent::unattended(event, launch_interrupt_epoch));
}

struct PowerDevice {
    fd: RawFd,
    grabbed: bool,
}

impl PowerDevice {
    fn open() -> io::Result<Self> {
        for index in 0..32 {
            let name =
                std::fs::read_to_string(format!("/sys/class/input/event{index}/device/name"))
                    .unwrap_or_default()
                    .to_lowercase();
            if !name.contains("pwrkey")
                && !name.contains("powerkey")
                && !name.contains("power button")
            {
                continue;
            }
            let path = CString::new(format!("/dev/input/event{index}")).unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(Self { fd, grabbed: false });
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "power key input not found",
        ))
    }

    fn set_grab(&mut self, grab: bool) -> io::Result<()> {
        if self.grabbed == grab {
            return Ok(());
        }
        let result = unsafe { libc::ioctl(self.fd, EVIOCGRAB, i32::from(grab)) };
        if result == 0 {
            self.grabbed = grab;
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn force_release_and_reopen(&mut self) -> io::Result<()> {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
        self.grabbed = false;
        let replacement = PowerDevice::open()?;
        *self = replacement;
        Ok(())
    }

    fn drain(&mut self) -> Vec<i32> {
        let event_size = std::mem::size_of::<libc::timeval>() + 8;
        let mut values = Vec::new();
        let mut buffer = [0u8; 24 * 16];
        loop {
            let count = unsafe {
                libc::read(
                    self.fd,
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                    buffer.len(),
                )
            };
            if count <= 0 {
                break;
            }
            for event in buffer[..count as usize].chunks_exact(event_size) {
                let offset = event_size - 8;
                let kind = u16::from_ne_bytes([event[offset], event[offset + 1]]);
                let code = u16::from_ne_bytes([event[offset + 2], event[offset + 3]]);
                let value = i32::from_ne_bytes(event[offset + 4..offset + 8].try_into().unwrap());
                if kind == EV_KEY && code == KEY_POWER && matches!(value, 0 | 1) {
                    values.push(value);
                }
            }
        }
        values
    }
}

impl Drop for PowerDevice {
    fn drop(&mut self) {
        let _ = self.set_grab(false);
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
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

    #[test]
    fn raw_power_press_invalidates_a_pending_auto_resuspend_immediately() {
        let epoch = AtomicU64::new(23);
        assert_eq!(note_power_press(&epoch), 24);
        assert_eq!(epoch.load(std::sync::atomic::Ordering::Acquire), 24);
    }

    #[test]
    fn guarded_press_before_suspend_still_invalidates_the_transaction() {
        let t = Instant::now();
        let epoch = AtomicU64::new(41);
        let mut guard = WakeGuard::default();
        guard.arm().unwrap();

        assert!(consume_wake_guard_event(&mut guard, 1, t, &epoch));
        assert_eq!(epoch.load(std::sync::atomic::Ordering::Acquire), 42);

        guard.resume(t + Duration::from_millis(10)).unwrap();
        assert!(consume_wake_guard_event(
            &mut guard,
            0,
            t + Duration::from_millis(20),
            &epoch,
        ));
        assert_eq!(epoch.load(std::sync::atomic::Ordering::Acquire), 42);
    }
}
