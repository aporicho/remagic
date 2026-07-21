use super::queue::{is_input_boundary, InputPush};
use super::state::ForegroundLease;
use super::*;
use crate::panel::{PanelCommand, RefreshIntent};
use crate::protocol::{
    input_packet, INPUT_PEN_PRESS, INPUT_PEN_RELEASE, INPUT_PEN_UPDATE, INPUT_TOUCH_PRESS,
    INPUT_TOUCH_RELEASE, INPUT_TOUCH_UPDATE, REFRESH_MODE_FAST,
};
use std::io;
use std::sync::{mpsc, Arc};

#[test]
fn foreground_requires_a_connected_surface_and_nonzero_fence() {
    let (tx, _rx) = mpsc::sync_channel(1024);
    let state = HostState::new(tx, 960, 1696, 3840);
    assert_eq!(
        state.set_foreground(10, 1, 1, true).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        state.set_foreground(10, 0, 1, true).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn background_surface_cannot_refresh_the_visible_application() {
    let (tx, rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    state.request_surface_full_refresh(22).unwrap();
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 11,
        generation: 2,
        epoch: 3,
        ink_enabled: false,
    });
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 44,
        generation: 5,
        foreground_epoch: 6,
    });
    state.request_surface_full_refresh(22).unwrap();
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    state.request_surface_full_refresh(11).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::FullRefresh { lease }
            if lease.key == 11 && lease.generation == 2 && lease.foreground_epoch == 3
    ));
}

#[test]
fn qtfb_fast_mode_is_a_quality_monochrome_partial_update() {
    let (tx, rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(22, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::RegisterSurface(_)
    ));
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 22,
        generation: 2,
        epoch: 3,
        ink_enabled: false,
    });
    state.set_refresh_mode(22, REFRESH_MODE_FAST).unwrap();
    state
        .commit_damage(22, Some(crate::geometry::Rect::new(4, 5, 6, 7)))
        .unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::Damage {
            intent: RefreshIntent::MonoQuality,
            ..
        }
    ));
}

#[test]
fn a_surface_key_has_exactly_one_live_client_owner() {
    let (tx, _rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(22, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    state
        .activate_client(22, 10, queue::InputQueue::new(8))
        .unwrap();
    let error = state
        .register(22, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .err()
        .expect("duplicate owner must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    state.unregister(10, Some(22));
    assert!(state
        .register(22, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .is_ok());
}

#[test]
fn surface_receives_no_input_until_initialize_reply_phase_completes() {
    let (tx, _rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(44, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 44,
        generation: 5,
        epoch: 6,
        ink_enabled: false,
    });
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 44,
        generation: 5,
        foreground_epoch: 6,
    });
    state.inject_tap(10, 10).unwrap();

    let input = queue::InputQueue::new(8);
    state.activate_client(44, 99, Arc::clone(&input)).unwrap();
    state.inject_tap(20, 20).unwrap();
    let first = input.pop().unwrap();
    assert_eq!(
        i32::from_le_bytes(first[16..20].try_into().unwrap()),
        20,
        "input queued before initialize reply/activation"
    );
    input.close();
}

#[test]
fn requested_foreground_does_not_receive_input_until_panel_commit() {
    let (tx, _rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(44, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    let input = queue::InputQueue::new(8);
    state.activate_client(44, 99, Arc::clone(&input)).unwrap();
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 44,
        generation: 5,
        epoch: 6,
        ink_enabled: false,
    });

    state.inject_tap(10, 10).unwrap();
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 44,
        generation: 5,
        foreground_epoch: 6,
    });
    state.inject_tap(20, 20).unwrap();

    let first = input.pop().unwrap();
    assert_eq!(
        i32::from_le_bytes(first[16..20].try_into().unwrap()),
        20,
        "input was routed before the panel committed the foreground lease"
    );
    input.close();
}

#[test]
fn previous_committed_foreground_stops_receiving_input_when_switch_is_requested() {
    let (tx, _rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    for key in [44, 55] {
        state
            .register(key, 64, 64, crate::protocol::PixelFormat::Rgb565)
            .unwrap();
    }
    let old_input = queue::InputQueue::new(8);
    state
        .activate_client(44, 99, Arc::clone(&old_input))
        .unwrap();
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 44,
        generation: 5,
        foreground_epoch: 6,
    });
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 55,
        generation: 7,
        epoch: 8,
        ink_enabled: false,
    });

    state.inject_tap(10, 10).unwrap();
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 44,
        generation: 5,
        epoch: 6,
        ink_enabled: false,
    });
    state.inject_tap(20, 20).unwrap();

    let first = old_input.pop().unwrap();
    assert_eq!(
        i32::from_le_bytes(first[16..20].try_into().unwrap()),
        20,
        "the old application received input after a new foreground was requested"
    );
    old_input.close();
}

#[test]
fn press_and_release_are_never_classed_as_droppable_moves() {
    for input_type in [
        INPUT_PEN_PRESS,
        INPUT_PEN_RELEASE,
        INPUT_TOUCH_PRESS,
        INPUT_TOUCH_RELEASE,
    ] {
        assert!(is_input_boundary(&input_packet(input_type, 1, 2, 3, 4)));
    }
    for input_type in [INPUT_PEN_UPDATE, INPUT_TOUCH_UPDATE] {
        assert!(!is_input_boundary(&input_packet(input_type, 1, 2, 3, 4)));
    }
}

#[test]
fn bounded_input_queue_preserves_boundaries_in_fifo_order() {
    let queue = queue::InputQueue::new(4);
    let down = input_packet(INPUT_PEN_PRESS, 1, 10, 10, 20);
    let move_one = input_packet(INPUT_PEN_UPDATE, 1, 20, 20, 20);
    let move_two = input_packet(INPUT_PEN_UPDATE, 1, 30, 30, 20);
    let move_three = input_packet(INPUT_PEN_UPDATE, 1, 40, 40, 20);
    let up = input_packet(INPUT_PEN_RELEASE, 1, 40, 40, 0);
    assert_eq!(queue.push(down), InputPush::Queued);
    assert_eq!(queue.push(move_one), InputPush::Queued);
    assert_eq!(queue.push(move_two), InputPush::Queued);
    assert_eq!(queue.push(move_three), InputPush::Queued);
    assert_eq!(queue.push(up), InputPush::Coalesced);

    let packets = (0..4).map(|_| queue.pop().unwrap()).collect::<Vec<_>>();
    assert_eq!(packets.first(), Some(&down));
    assert_eq!(packets.last(), Some(&up));
    assert!(packets[1..3]
        .iter()
        .all(|packet| !is_input_boundary(packet)));
    queue.close();
}
