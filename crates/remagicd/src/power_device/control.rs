use super::Control;
use std::io;
use std::os::fd::RawFd;
use std::sync::{mpsc, Arc};

#[derive(Debug)]
pub struct ControlSender {
    sender: mpsc::Sender<Control>,
    wake: Option<Arc<WakeEvent>>,
}

impl Clone for ControlSender {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl ControlSender {
    pub(super) fn new(sender: mpsc::Sender<Control>, wake: Arc<WakeEvent>) -> Self {
        Self {
            sender,
            wake: Some(wake),
        }
    }

    pub fn send(&self, control: Control) -> Result<(), mpsc::SendError<Control>> {
        self.sender.send(control)?;
        if let Some(wake) = &self.wake {
            wake.notify();
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn from_test_channel(sender: mpsc::Sender<Control>) -> Self {
        Self { sender, wake: None }
    }
}

#[derive(Debug)]
pub(super) struct WakeEvent(RawFd);

impl WakeEvent {
    pub(super) fn create() -> io::Result<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(fd))
        }
    }

    pub(super) fn fd(&self) -> RawFd {
        self.0
    }

    fn notify(&self) {
        let value = 1_u64.to_ne_bytes();
        let _ = unsafe { libc::write(self.0, value.as_ptr().cast(), value.len()) };
    }

    pub(super) fn drain(&self) {
        let mut value = 0_u64;
        let _ = unsafe {
            libc::read(
                self.0,
                (&mut value as *mut u64).cast(),
                std::mem::size_of::<u64>(),
            )
        };
    }
}

impl Drop for WakeEvent {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}
