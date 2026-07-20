use remagic_core::power::{ClickTracker, PowerAction};
use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tokio_mpsc;

use crate::Event;

const EV_KEY: u16 = 1;
const KEY_POWER: u16 = 116;
const EVIOCGRAB: libc::c_ulong = 0x40044590;

#[derive(Clone, Copy, Debug)]
pub enum Control {
    Grab(bool),
}

pub fn spawn(
    events: tokio_mpsc::Sender<Event>,
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
        loop {
            while let Ok(control) = control_rx.try_recv() {
                match control {
                    Control::Grab(grab) => device.set_grab(grab),
                }
            }
            for value in device.drain() {
                let now = Instant::now();
                let action = if value == 1 {
                    tracker.press(now);
                    PowerAction::None
                } else if value == 0 {
                    tracker.release(now)
                } else {
                    PowerAction::None
                };
                send_action(action, &events);
            }
            send_action(tracker.poll(Instant::now()), &events);
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    (thread, control_tx)
}

fn send_action(action: PowerAction, events: &tokio_mpsc::Sender<Event>) {
    let event = match action {
        PowerAction::None => return,
        PowerAction::Single => Event::SinglePower,
        PowerAction::Triple => Event::TriplePower,
        PowerAction::Long => Event::LongPower,
    };
    let _ = events.blocking_send(event);
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

    fn set_grab(&mut self, grab: bool) {
        if self.grabbed == grab {
            return;
        }
        let result = unsafe { libc::ioctl(self.fd, EVIOCGRAB, i32::from(grab)) };
        if result == 0 {
            self.grabbed = grab;
        } else {
            eprintln!(
                "remagicd: EVIOCGRAB({grab}) failed: {}",
                io::Error::last_os_error()
            );
        }
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
        self.set_grab(false);
        unsafe { libc::close(self.fd) };
    }
}
