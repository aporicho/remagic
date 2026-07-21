use crate::protocol::{
    INPUT_PEN_PRESS, INPUT_PEN_RELEASE, INPUT_TOUCH_PRESS, INPUT_TOUCH_RELEASE,
    QTFB_SERVER_MESSAGE_SIZE,
};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

pub(super) const INPUT_QUEUE_CAPACITY: usize = 512;

pub(super) struct InputQueue {
    state: Mutex<InputQueueState>,
    ready: Condvar,
    capacity: usize,
}

#[derive(Default)]
struct InputQueueState {
    packets: VecDeque<[u8; QTFB_SERVER_MESSAGE_SIZE]>,
    closed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputPush {
    Queued,
    Coalesced,
    Closed,
    BoundaryOverflow,
}

impl InputQueue {
    pub(super) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(InputQueueState::default()),
            ready: Condvar::new(),
            capacity: capacity.max(1),
        })
    }

    pub(super) fn push(&self, packet: [u8; QTFB_SERVER_MESSAGE_SIZE]) -> InputPush {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return InputPush::Closed;
        }
        let mut outcome = InputPush::Queued;
        if state.packets.len() >= self.capacity {
            // Preserve every boundary in FIFO order. Only old coalescible
            // moves may be evicted under pressure.
            if let Some(index) = state
                .packets
                .iter()
                .position(|candidate| !is_input_boundary(candidate))
            {
                state.packets.remove(index);
                outcome = InputPush::Coalesced;
            } else {
                state.closed = true;
                self.ready.notify_all();
                return InputPush::BoundaryOverflow;
            }
        }
        state.packets.push_back(packet);
        self.ready.notify_one();
        outcome
    }

    pub(super) fn pop(&self) -> Option<[u8; QTFB_SERVER_MESSAGE_SIZE]> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(packet) = state.packets.pop_front() {
                return Some(packet);
            }
            if state.closed {
                return None;
            }
            let (next, _) = self
                .ready
                .wait_timeout(state, std::time::Duration::from_millis(100))
                .unwrap();
            state = next;
        }
    }

    pub(super) fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        self.ready.notify_all();
    }
}

pub(super) fn is_input_boundary(packet: &[u8; QTFB_SERVER_MESSAGE_SIZE]) -> bool {
    if packet[0] != crate::protocol::MESSAGE_USERINPUT {
        return false;
    }
    let input_type = i32::from_le_bytes(packet[8..12].try_into().unwrap());
    matches!(
        input_type,
        INPUT_PEN_PRESS | INPUT_PEN_RELEASE | INPUT_TOUCH_PRESS | INPUT_TOUCH_RELEASE
    )
}
