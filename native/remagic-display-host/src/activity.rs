//! Best-effort interactive activity reporting to the ReMagic power policy.
//!
//! Input delivery never waits for this side channel. A missing supervisor is
//! retried with a bounded backoff and the current frame continues normally.

use crate::input::{CapturedInput, InputFrame, PenPhase, TouchPhase};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const SOCKET_PATH: &str = "/run/remagic/activity.sock";
const RETRY_DELAY: Duration = Duration::from_secs(5);

pub struct ActivityReporter {
    stream: Option<UnixStream>,
    retry_after: Instant,
}

impl ActivityReporter {
    pub fn new() -> Self {
        Self {
            stream: None,
            retry_after: Instant::now(),
        }
    }

    pub fn observe(&mut self, captured: &CapturedInput) {
        if !is_new_interaction(captured) {
            return;
        }
        if self.stream.is_none() && Instant::now() >= self.retry_after {
            match UnixStream::connect(SOCKET_PATH) {
                Ok(stream) => {
                    let _ = stream.set_nonblocking(true);
                    self.stream = Some(stream);
                }
                Err(_) => {
                    self.retry_after = Instant::now() + RETRY_DELAY;
                    return;
                }
            }
        }
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        if let Err(error) = stream.write(&[1]) {
            if error.kind() != io::ErrorKind::WouldBlock {
                self.stream = None;
                self.retry_after = Instant::now() + RETRY_DELAY;
            }
        }
    }
}

impl Default for ActivityReporter {
    fn default() -> Self {
        Self::new()
    }
}

fn is_new_interaction(captured: &CapturedInput) -> bool {
    matches!(
        captured.frame,
        InputFrame::Pen(frame) if frame.phase == PenPhase::Down
    ) || matches!(
        captured.frame,
        InputFrame::Touch(frame) if frame.phase == TouchPhase::Down
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{PenFrame, PenTool};

    #[test]
    fn only_the_start_of_an_interaction_resets_idle_time() {
        let frame = |phase| CapturedInput {
            epoch: 1,
            frame: InputFrame::Pen(PenFrame {
                sequence: 1,
                kernel_time_ns: 1,
                phase,
                tool: PenTool::Pen,
                x: 0,
                y: 0,
                pressure: 0,
                pressure_max: 1,
            }),
        };
        assert!(is_new_interaction(&frame(PenPhase::Down)));
        assert!(!is_new_interaction(&frame(PenPhase::Move)));
        assert!(!is_new_interaction(&frame(PenPhase::Up)));
    }
}
