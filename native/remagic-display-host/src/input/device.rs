use super::{
    AxisRange, InputFrame, MarkerDecoder, RawEvent, TouchDecoder, EVIOCGABS_MT_PRESSURE,
    EVIOCGABS_MT_SLOT, EVIOCGABS_MT_X, EVIOCGABS_MT_Y, EVIOCGABS_PRESSURE, EVIOCGABS_X,
    EVIOCGABS_Y, EVIOCGRAB,
};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxInputEvent {
    time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

pub struct InputThreads {
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl InputThreads {
    pub fn spawn(
        logical_width: i32,
        logical_height: i32,
        tx: Sender<InputFrame>,
    ) -> io::Result<Self> {
        let marker = find_input_device("Elan marker input")?;
        let touch = find_input_device("Elan touch input")?;
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let marker_stop = Arc::clone(&stop);
        let marker_failed = Arc::clone(&failed);
        let marker_health_stop = Arc::clone(&stop);
        let marker_tx = tx.clone();
        let marker_handle = std::thread::Builder::new()
            .name("remagic-marker".into())
            .spawn(move || {
                if let Err(error) = run_marker(
                    &marker,
                    logical_width,
                    logical_height,
                    marker_stop,
                    marker_tx,
                ) {
                    eprintln!("remagic-display-host: marker input stopped: {error}");
                    if !marker_health_stop.load(Ordering::Acquire) {
                        marker_failed.store(true, Ordering::Release);
                    }
                }
            })?;
        let touch_stop = Arc::clone(&stop);
        let touch_failed = Arc::clone(&failed);
        let touch_health_stop = Arc::clone(&stop);
        let touch_handle = std::thread::Builder::new()
            .name("remagic-touch".into())
            .spawn(move || {
                if let Err(error) = run_touch(&touch, logical_width, logical_height, touch_stop, tx)
                {
                    eprintln!("remagic-display-host: touch input stopped: {error}");
                    if !touch_health_stop.load(Ordering::Acquire) {
                        touch_failed.store(true, Ordering::Release);
                    }
                }
            })?;
        Ok(Self {
            stop,
            failed,
            handles: vec![marker_handle, touch_handle],
        })
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl Drop for InputThreads {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn find_input_device(name: &str) -> io::Result<PathBuf> {
    let text = fs::read_to_string("/proc/bus/input/devices")?;
    for block in text.split("\n\n") {
        if !block
            .lines()
            .any(|line| line == format!("N: Name=\"{name}\""))
        {
            continue;
        }
        let handlers = block
            .lines()
            .find_map(|line| line.strip_prefix("H: Handlers="))
            .unwrap_or_default();
        if let Some(event) = handlers
            .split_whitespace()
            .find(|value| value.starts_with("event"))
        {
            return Ok(PathBuf::from("/dev/input").join(event));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("input device {name} not found"),
    ))
}

fn run_marker(
    path: &Path,
    logical_width: i32,
    logical_height: i32,
    stop: Arc<AtomicBool>,
    tx: Sender<InputFrame>,
) -> io::Result<()> {
    let fd = open_grabbed(path)?;
    let mut decoder = MarkerDecoder::new(
        logical_width,
        logical_height,
        query_axis(fd.as_raw_fd(), EVIOCGABS_X, 0, 6760),
        query_axis(fd.as_raw_fd(), EVIOCGABS_Y, 0, 11960),
        query_axis(fd.as_raw_fd(), EVIOCGABS_PRESSURE, 0, 4096),
    );
    read_events(fd.as_raw_fd(), stop, |event| {
        if let Some(frame) = decoder.consume(event) {
            let _ = tx.send(InputFrame::Pen(frame));
        }
    })
}

fn run_touch(
    path: &Path,
    logical_width: i32,
    logical_height: i32,
    stop: Arc<AtomicBool>,
    tx: Sender<InputFrame>,
) -> io::Result<()> {
    let fd = open_grabbed(path)?;
    let slot_range = query_axis(fd.as_raw_fd(), EVIOCGABS_MT_SLOT, 0, 9);
    let _pressure = query_axis(fd.as_raw_fd(), EVIOCGABS_MT_PRESSURE, 0, 255);
    let mut decoder = TouchDecoder::new(
        logical_width,
        logical_height,
        query_axis(fd.as_raw_fd(), EVIOCGABS_MT_X, 0, 1248),
        query_axis(fd.as_raw_fd(), EVIOCGABS_MT_Y, 0, 2208),
        (slot_range.maximum - slot_range.minimum + 1).max(1) as usize,
    );
    read_events(fd.as_raw_fd(), stop, |event| {
        for frame in decoder.consume(event) {
            let _ = tx.send(InputFrame::Touch(frame));
        }
    })
}

fn open_grabbed(path: &Path) -> io::Result<OwnedFd> {
    let bytes = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "input path contains NUL"))?;
    let raw = unsafe {
        libc::open(
            bytes.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let one: libc::c_int = 1;
    if unsafe { libc::ioctl(fd.as_raw_fd(), EVIOCGRAB, one) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

fn query_axis(
    fd: RawFd,
    request: libc::c_ulong,
    fallback_min: i32,
    fallback_max: i32,
) -> AxisRange {
    let mut info = InputAbsInfo::default();
    if unsafe { libc::ioctl(fd, request, &mut info) } == 0 && info.maximum > info.minimum {
        AxisRange {
            minimum: info.minimum,
            maximum: info.maximum,
        }
    } else {
        AxisRange {
            minimum: fallback_min,
            maximum: fallback_max,
        }
    }
}

fn read_events(
    fd: RawFd,
    stop: Arc<AtomicBool>,
    mut consume: impl FnMut(RawEvent),
) -> io::Result<()> {
    let mut events = [LinuxInputEvent {
        time: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        event_type: 0,
        code: 0,
        value: 0,
    }; 64];
    while !stop.load(Ordering::Acquire) {
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let status = unsafe { libc::poll(&mut descriptor, 1, 100) };
        if status < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if status == 0 {
            continue;
        }
        let bytes = unsafe {
            libc::read(
                fd,
                events.as_mut_ptr().cast(),
                std::mem::size_of_val(&events),
            )
        };
        if bytes < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                continue;
            }
            return Err(error);
        }
        let count = bytes as usize / std::mem::size_of::<LinuxInputEvent>();
        for event in &events[..count] {
            let seconds = event.time.tv_sec.max(0) as u64;
            let micros = event.time.tv_usec.max(0) as u64;
            consume(RawEvent {
                time_ns: seconds
                    .saturating_mul(1_000_000_000)
                    .saturating_add(micros.saturating_mul(1_000)),
                event_type: event.event_type,
                code: event.code,
                value: event.value,
            });
        }
    }
    let zero: libc::c_int = 0;
    let _ = unsafe { libc::ioctl(fd, EVIOCGRAB, zero) };
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}
