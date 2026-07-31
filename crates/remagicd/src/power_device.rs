use remagic_core::power::{ClickTracker, PowerAction};
use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tokio_mpsc;

use crate::daemon::{Event, QueuedEvent};

mod control;
mod wake_guard;
pub use control::ControlSender;
use control::WakeEvent;
pub use wake_guard::WakeGesture;
use wake_guard::WakeGuard;

const EV_KEY: u16 = 1;
const EV_SW: u16 = 5;
const KEY_POWER: u16 = 116;
const SW_LID: u16 = 0;
const EVIOCGRAB: libc::c_ulong = 0x40044590;

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
        reply: tokio::sync::oneshot::Sender<Result<WakeGesture, String>>,
    },
    CancelWakeGuard {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

pub fn spawn(
    events: tokio_mpsc::Sender<QueuedEvent>,
    launch_interrupt_epoch: Arc<AtomicU64>,
    cover_closed: Arc<AtomicBool>,
) -> (std::thread::JoinHandle<()>, ControlSender) {
    let (control_tx, control_rx) = mpsc::channel();
    let wake = Arc::new(WakeEvent::create().expect("eventfd is required for power control"));
    let thread_wake = Arc::clone(&wake);
    let thread = std::thread::spawn(move || {
        InputThread::new(
            events,
            launch_interrupt_epoch,
            cover_closed,
            control_rx,
            thread_wake,
        )
        .run();
    });
    (thread, ControlSender::new(control_tx, wake))
}

struct InputThread {
    events: tokio_mpsc::Sender<QueuedEvent>,
    launch_interrupt_epoch: Arc<AtomicU64>,
    cover_closed: Arc<AtomicBool>,
    control_rx: mpsc::Receiver<Control>,
    thread_wake: Arc<WakeEvent>,
    device: PowerDevice,
    tracker: ClickTracker,
    wake_guard: WakeGuard,
    cover: Option<CoverDevice>,
    observed_cover_closed: Option<bool>,
}

impl InputThread {
    fn new(
        events: tokio_mpsc::Sender<QueuedEvent>,
        launch_interrupt_epoch: Arc<AtomicU64>,
        cover_closed: Arc<AtomicBool>,
        control_rx: mpsc::Receiver<Control>,
        thread_wake: Arc<WakeEvent>,
    ) -> Self {
        let mut thread = Self {
            events,
            launch_interrupt_epoch,
            cover_closed,
            control_rx,
            thread_wake,
            device: open_power_device(),
            tracker: ClickTracker::default(),
            wake_guard: WakeGuard::default(),
            cover: open_cover_device(),
            observed_cover_closed: None,
        };
        thread.observe_initial_cover();
        thread
    }

    fn run(&mut self) {
        loop {
            wait_for_input_or_control(
                self.device.fd,
                self.cover.as_ref().map_or(-1, |device| device.fd),
                self.thread_wake.fd(),
                next_deadline(&self.tracker, &self.wake_guard),
            );
            self.thread_wake.drain();
            self.handle_controls();
            self.handle_power_events();
            self.handle_cover_events();
            self.poll_pending_click();
        }
    }

    fn observe_initial_cover(&mut self) {
        let Some(cover_device) = &self.cover else {
            return;
        };
        match cover_device.initial_closed() {
            Ok(closed) => {
                self.observed_cover_closed = Some(closed);
                self.cover_closed
                    .store(closed, std::sync::atomic::Ordering::Release);
                if closed {
                    self.send_cover_event(Event::CoverClosed);
                }
            }
            Err(error) => eprintln!("remagicd: cannot query initial cover state: {error}"),
        }
    }

    fn handle_controls(&mut self) {
        while let Ok(control) = self.control_rx.try_recv() {
            match control {
                Control::Grab { grab, reply } => self.handle_grab(grab, reply),
                Control::ArmWakeGuard { reply } => self.handle_arm_wake_guard(reply),
                Control::ResumeWakeGuard { reply } => {
                    self.wake_guard.resume_and_report(Instant::now(), reply);
                }
                Control::CancelWakeGuard { reply } => {
                    self.tracker.clear();
                    self.wake_guard.cancel();
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }

    fn handle_grab(&mut self, grab: bool, reply: tokio::sync::oneshot::Sender<Result<(), String>>) {
        self.tracker.clear();
        self.wake_guard.cancel();
        let mut result = self.device.set_grab(grab);
        if result.is_err() && !grab {
            result = self.device.force_release_and_reopen();
        }
        let _ = reply.send(result.map_err(|error| error.to_string()));
    }

    fn handle_arm_wake_guard(&mut self, reply: tokio::sync::oneshot::Sender<Result<(), String>>) {
        self.tracker.clear();
        let result = if self.device.grabbed {
            self.wake_guard.arm()
        } else {
            Err("cannot arm wake guard while the power key is not grabbed".into())
        };
        let _ = reply.send(result);
    }

    fn handle_power_events(&mut self) {
        for value in self.device.drain() {
            let now = Instant::now();
            if consume_wake_guard_event(
                &mut self.wake_guard,
                value,
                now,
                &self.launch_interrupt_epoch,
            ) {
                continue;
            }
            let action = self.track_power_value(value, now);
            send_action(action, &self.events, &self.launch_interrupt_epoch);
        }
    }

    fn track_power_value(&mut self, value: i32, now: Instant) -> PowerAction {
        match value {
            1 => {
                note_power_press(&self.launch_interrupt_epoch);
                let _ = self.events.blocking_send(QueuedEvent::unattended(
                    Event::UserActivity,
                    &self.launch_interrupt_epoch,
                ));
                self.tracker.press(now);
                PowerAction::None
            }
            0 => self.tracker.release(now),
            _ => PowerAction::None,
        }
    }

    fn handle_cover_events(&mut self) {
        let states = {
            let Some(cover_device) = &mut self.cover else {
                return;
            };
            cover_device.drain()
        };
        for closed in states {
            if self.observed_cover_closed == Some(closed) {
                continue;
            }
            self.observed_cover_closed = Some(closed);
            self.cover_closed
                .store(closed, std::sync::atomic::Ordering::Release);
            self.send_cover_event(if closed {
                Event::CoverClosed
            } else {
                Event::CoverOpened
            });
        }
    }

    fn send_cover_event(&self, event: Event) {
        let _ = self
            .events
            .blocking_send(QueuedEvent::unattended(event, &self.launch_interrupt_epoch));
    }

    fn poll_pending_click(&mut self) {
        let now = Instant::now();
        self.wake_guard.poll(now);
        if !self.wake_guard.active {
            send_action(
                self.tracker.poll(now),
                &self.events,
                &self.launch_interrupt_epoch,
            );
        }
    }
}

fn open_power_device() -> PowerDevice {
    loop {
        match PowerDevice::open() {
            Ok(device) => return device,
            Err(error) => {
                eprintln!("remagicd: power device unavailable: {error}");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn open_cover_device() -> Option<CoverDevice> {
    CoverDevice::open().map_or_else(
        |error| {
            eprintln!("remagicd: cover sensor unavailable: {error}");
            None
        },
        Some,
    )
}

fn next_deadline(tracker: &ClickTracker, wake_guard: &WakeGuard) -> Option<Instant> {
    match (tracker.next_deadline(), wake_guard.next_deadline()) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

fn wait_for_input_or_control(
    device_fd: RawFd,
    cover_fd: RawFd,
    wake_fd: RawFd,
    deadline: Option<Instant>,
) {
    let timeout = deadline.map_or(-1, |deadline| {
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, i32::MAX as u128) as i32
    });
    let mut descriptors = [
        libc::pollfd {
            fd: device_fd,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: cover_fd,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
    ];
    loop {
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout,
            )
        };
        if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

fn input_event_size() -> usize {
    std::mem::size_of::<libc::timeval>() + 8
}

fn eviocgsw(length: usize) -> libc::c_ulong {
    const IOC_NRBITS: libc::c_ulong = 8;
    const IOC_TYPEBITS: libc::c_ulong = 8;
    const IOC_SIZEBITS: libc::c_ulong = 14;
    const IOC_NRSHIFT: libc::c_ulong = 0;
    const IOC_TYPESHIFT: libc::c_ulong = IOC_NRSHIFT + IOC_NRBITS;
    const IOC_SIZESHIFT: libc::c_ulong = IOC_TYPESHIFT + IOC_TYPEBITS;
    const IOC_DIRSHIFT: libc::c_ulong = IOC_SIZESHIFT + IOC_SIZEBITS;
    const IOC_READ: libc::c_ulong = 2;

    (IOC_READ << IOC_DIRSHIFT)
        | ((b'E' as libc::c_ulong) << IOC_TYPESHIFT)
        | (0x1b << IOC_NRSHIFT)
        | ((length as libc::c_ulong) << IOC_SIZESHIFT)
}

fn read_input_values(fd: RawFd, expected_kind: u16, expected_code: u16) -> Vec<i32> {
    let event_size = input_event_size();
    let mut values = Vec::new();
    let mut buffer = [0u8; 24 * 16];
    loop {
        let count =
            unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
        if count <= 0 {
            break;
        }
        for event in buffer[..count as usize].chunks_exact(event_size) {
            if let Some(value) = parse_input_event(event, expected_kind, expected_code) {
                values.push(value);
            }
        }
    }
    values
}

fn parse_input_event(event: &[u8], expected_kind: u16, expected_code: u16) -> Option<i32> {
    let event_size = input_event_size();
    if event.len() != event_size {
        return None;
    }
    let offset = event_size - 8;
    let kind = u16::from_ne_bytes([event[offset], event[offset + 1]]);
    let code = u16::from_ne_bytes([event[offset + 2], event[offset + 3]]);
    let value = i32::from_ne_bytes(event[offset + 4..offset + 8].try_into().ok()?);
    (kind == expected_kind && code == expected_code).then_some(value)
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
    if wake_guard.is_armed_before_resume() && value == 1 {
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
        read_input_values(self.fd, EV_KEY, KEY_POWER)
            .into_iter()
            .filter(|value| matches!(value, 0 | 1))
            .collect()
    }
}

struct CoverDevice {
    fd: RawFd,
}

impl CoverDevice {
    fn open() -> io::Result<Self> {
        for index in 0..32 {
            let name =
                std::fs::read_to_string(format!("/sys/class/input/event{index}/device/name"))
                    .unwrap_or_default()
                    .to_lowercase();
            if !name.contains("hall") {
                continue;
            }
            let path = CString::new(format!("/dev/input/event{index}")).unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(Self { fd });
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Hall effect input not found",
        ))
    }

    fn drain(&mut self) -> Vec<bool> {
        read_input_values(self.fd, EV_SW, SW_LID)
            .into_iter()
            .filter_map(|value| match value {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            })
            .collect()
    }

    fn initial_closed(&self) -> io::Result<bool> {
        let mut switches: libc::c_ulong = 0;
        let result = unsafe {
            libc::ioctl(
                self.fd,
                eviocgsw(std::mem::size_of_val(&switches)),
                &mut switches,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((switches & (1 << SW_LID)) != 0)
    }
}

impl Drop for CoverDevice {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
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

    #[test]
    fn parses_only_lid_switch_events_for_cover_state() {
        let mut event = vec![0_u8; input_event_size()];
        let offset = input_event_size() - 8;
        event[offset..offset + 2].copy_from_slice(&EV_SW.to_ne_bytes());
        event[offset + 2..offset + 4].copy_from_slice(&SW_LID.to_ne_bytes());
        event[offset + 4..offset + 8].copy_from_slice(&1_i32.to_ne_bytes());

        assert_eq!(parse_input_event(&event, EV_SW, SW_LID), Some(1));

        event[offset + 2..offset + 4].copy_from_slice(&15_u16.to_ne_bytes());
        assert_eq!(parse_input_event(&event, EV_SW, SW_LID), None);

        event[offset + 2..offset + 4].copy_from_slice(&SW_LID.to_ne_bytes());
        event[offset..offset + 2].copy_from_slice(&EV_KEY.to_ne_bytes());
        assert_eq!(parse_input_event(&event, EV_SW, SW_LID), None);
    }

    #[test]
    fn constructs_linux_switch_state_ioctl() {
        assert_eq!(eviocgsw(std::mem::size_of::<libc::c_ulong>()), 0x8008451b);
    }
}
