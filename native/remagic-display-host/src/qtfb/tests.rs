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
fn direct_ink_configuration_is_fenced_and_reflected_in_status() {
    let (tx, rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
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

    let stale = state.configure_ink(44, 4, 6, true, None).unwrap_err();
    assert_eq!(stale.kind(), io::ErrorKind::PermissionDenied);
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

    state.telemetry.commit_ink(
        crate::panel::PanelLease {
            key: 44,
            generation: 5,
            foreground_epoch: 6,
        },
        true,
    );
    state.configure_ink(44, 5, 6, true, None).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::ConfigureInk {
            lease,
            enabled: true,
            region: None,
        } if lease.key == 44 && lease.generation == 5 && lease.foreground_epoch == 6
    ));
    assert!(state.snapshot().ink_enabled);

    state.telemetry.commit_ink(
        crate::panel::PanelLease {
            key: 44,
            generation: 5,
            foreground_epoch: 6,
        },
        false,
    );
    state.configure_ink(44, 5, 6, false, None).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::ConfigureInk {
            lease,
            enabled: false,
            region: None,
        } if lease.key == 44 && lease.generation == 5 && lease.foreground_epoch == 6
    ));
    assert!(!state.snapshot().ink_enabled);
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
fn failed_surface_rollback_stops_host_instead_of_diverging_from_panel() {
    let (tx, _rx) = mpsc::sync_channel(1);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(23, 64, 64, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    // RegisterSurface occupies the only queue slot, so DropSurface cannot be
    // committed. The host must fail closed rather than forget a surface the
    // panel worker still owns.
    state.abort_registration(23);
    assert!(state.is_shutdown());
    assert!(!state.surface_exists(23));
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
    assert!(state.inject_tap(10, 10).is_err());

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

    assert!(state.inject_tap(10, 10).is_err());
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

    assert!(state.inject_tap(10, 10).is_err());
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

#[test]
fn lock_transaction_freezes_damage_filters_input_and_rejects_epoch_replay() {
    let (tx, rx) = mpsc::sync_channel(32);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(44, 954, 1696, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::RegisterSurface(_)
    ));
    state.mark_commit(44).unwrap();
    let input = queue::InputQueue::new(16);
    state.activate_client(44, 99, Arc::clone(&input)).unwrap();
    let unlock = crate::geometry::Rect::new(150, 1010, 654, 126);

    state.show_lock(44, 5, 7, 11, unlock).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::ShowLock {
            lease,
            sleep_epoch: 11,
        } if lease == crate::panel::PanelLease {
            key: 44,
            generation: 5,
            foreground_epoch: 7,
        }
    ));
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 44,
        generation: 5,
        foreground_epoch: 7,
    });
    state.telemetry.commit_lock(11);
    assert!(state.snapshot().lock_committed);

    // Pen and touches outside the dedicated unlock rectangle are swallowed.
    assert!(state.inject_pen_line(200, 300, 220, 320, 3).is_err());
    assert!(state.inject_tap(20, 20).is_err());
    state.inject_tap(200, 1050).unwrap();
    let down = input.pop().unwrap();
    let up = input.pop().unwrap();
    assert_eq!(
        i32::from_le_bytes(down[8..12].try_into().unwrap()),
        INPUT_TOUCH_PRESS
    );
    assert_eq!(i32::from_le_bytes(down[16..20].try_into().unwrap()), 200);
    assert_eq!(
        i32::from_le_bytes(up[8..12].try_into().unwrap()),
        INPUT_TOUCH_RELEASE
    );

    // Only button feedback may update the frozen host-owned lock image.
    state
        .damage(
            44,
            crate::geometry::Rect::new(0, 0, 954, 1696),
            RefreshIntent::Ui,
        )
        .unwrap();
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    state.damage(44, unlock, RefreshIntent::Ui).unwrap();
    assert!(matches!(rx.recv().unwrap(), PanelCommand::Damage { rect, .. } if rect == unlock));

    state.refresh_lock(11).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::RefreshLock {
            sleep_epoch: 11,
            ..
        }
    ));

    let replacement_sequence = state.mark_commit(44).unwrap();

    let telemetry = Arc::clone(&state.telemetry);
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            rx.recv().unwrap(),
            PanelCommand::CancelLock {
                sleep_epoch: 11,
                ..
            }
        ));
        telemetry.clear_committed_lock(11);
        telemetry.record_lock_cancelled(11);
    });
    state.cancel_lock(11, replacement_sequence).unwrap();
    worker.join().unwrap();
    assert_eq!(state.snapshot().lock_epoch, 0);
    assert!(
        state.cancel_lock(11, replacement_sequence).is_ok(),
        "lost ACK retry is idempotent"
    );
    assert_eq!(
        state.show_lock(44, 5, 8, 11, unlock).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied,
        "a completed sleep epoch cannot be replayed"
    );
    input.close();
}

#[test]
fn cancelling_a_queued_show_waits_for_the_panel_transaction_barrier() {
    let (tx, rx) = mpsc::sync_channel(8);
    let state = HostState::new(tx, 960, 1696, 3840);
    state
        .register(44, 954, 1696, crate::protocol::PixelFormat::Rgb565)
        .unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::RegisterSurface(_)
    ));
    let unlock = crate::geometry::Rect::new(150, 1010, 654, 126);
    state.show_lock(44, 5, 7, 11, unlock).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::ShowLock {
            sleep_epoch: 11,
            ..
        }
    ));

    let replacement_sequence = state.mark_commit(44).unwrap();

    let cancelling = Arc::clone(&state);
    let waiter = std::thread::spawn(move || cancelling.cancel_lock(11, replacement_sequence));
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
        PanelCommand::CancelLock {
            sleep_epoch: 11,
            ..
        }
    ));
    assert_eq!(state.snapshot().lock_epoch, 11);
    state.telemetry.commit_lock(11);
    state.telemetry.clear_committed_lock(11);
    state.telemetry.record_lock_cancelled(11);
    waiter.join().unwrap().unwrap();
    assert_eq!(state.snapshot().lock_epoch, 0);
}

#[test]
fn prepared_foreground_blocks_old_input_and_commits_ink_with_one_command() {
    let (tx, rx) = mpsc::sync_channel(16);
    let state = HostState::new(tx, 960, 1696, 3840);
    for key in [44, 55] {
        state
            .register(key, 64, 64, crate::protocol::PixelFormat::Rgb565)
            .unwrap();
        assert!(matches!(
            rx.recv().unwrap(),
            PanelCommand::RegisterSurface(_)
        ));
        state.mark_commit(key).unwrap();
    }
    let old_input = queue::InputQueue::new(8);
    let new_input = queue::InputQueue::new(8);
    state
        .activate_client(44, 1, Arc::clone(&old_input))
        .unwrap();
    state
        .activate_client(55, 2, Arc::clone(&new_input))
        .unwrap();
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 44,
        generation: 3,
        epoch: 4,
        ink_enabled: false,
    });
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 44,
        generation: 3,
        foreground_epoch: 4,
    });

    state.prepare_foreground(55, 6, 8).unwrap();
    assert!(state.inject_tap(10, 10).is_err());
    old_input.close();
    assert!(old_input.pop().is_none());
    state
        .damage(
            55,
            crate::geometry::Rect::new(0, 0, 20, 20),
            RefreshIntent::Ui,
        )
        .unwrap();
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

    state.activate_foreground(55, 6, 8, true, true).unwrap();
    assert!(matches!(
        rx.recv().unwrap(),
        PanelCommand::ActivateForeground {
            lease,
            ink_enabled: true,
            full_refresh: true,
        } if lease == crate::panel::PanelLease {
            key: 55,
            generation: 6,
            foreground_epoch: 8,
        }
    ));
    // Requested-but-not-yet-panel-committed leases receive no input.
    assert!(state.inject_tap(20, 20).is_err());
    state.telemetry.commit_foreground(crate::panel::PanelLease {
        key: 55,
        generation: 6,
        foreground_epoch: 8,
    });
    state.inject_tap(30, 30).unwrap();
    let first = new_input.pop().unwrap();
    assert_eq!(i32::from_le_bytes(first[16..20].try_into().unwrap()), 30);
    new_input.close();
}

#[test]
fn queued_input_from_before_a_transition_cannot_cross_the_epoch_barrier() {
    let (tx, rx) = mpsc::sync_channel(16);
    let state = HostState::new(tx, 960, 1696, 3840);
    for key in [44, 55] {
        state
            .register(key, 64, 64, crate::protocol::PixelFormat::Rgb565)
            .unwrap();
        assert!(matches!(
            rx.recv().unwrap(),
            PanelCommand::RegisterSurface(_)
        ));
    }
    let input = queue::InputQueue::new(8);
    state.activate_client(44, 1, Arc::clone(&input)).unwrap();
    let lease = crate::panel::PanelLease {
        key: 44,
        generation: 3,
        foreground_epoch: 4,
    };
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: lease.key,
        generation: lease.generation,
        epoch: lease.foreground_epoch,
        ink_enabled: true,
    });
    state.telemetry.commit_foreground(lease);
    state.telemetry.commit_ink(lease, true);

    let old_epoch = state.input_epoch.load(std::sync::atomic::Ordering::Acquire);
    let down = crate::input::PenFrame {
        sequence: 1,
        kernel_time_ns: 1,
        phase: crate::input::PenPhase::Down,
        tool: crate::input::PenTool::Pen,
        x: 10,
        y: 12,
        pressure: 100,
        pressure_max: 4096,
    };
    let _ = state.dispatch_captured_input(crate::input::CapturedInput {
        epoch: old_epoch,
        frame: crate::input::InputFrame::Pen(down),
    });
    assert!(
        matches!(rx.recv().unwrap(), PanelCommand::Pen { frame, .. } if frame.phase == crate::input::PenPhase::Down)
    );

    state.prepare_foreground(55, 6, 8).unwrap();
    assert!(
        matches!(rx.recv().unwrap(), PanelCommand::Pen { frame, .. } if frame.phase == crate::input::PenPhase::Cancel)
    );

    let _ = state.dispatch_captured_input(crate::input::CapturedInput {
        epoch: old_epoch,
        frame: crate::input::InputFrame::Pen(crate::input::PenFrame {
            sequence: 2,
            phase: crate::input::PenPhase::Move,
            x: 20,
            y: 22,
            ..down
        }),
    });
    input.close();
    let press = input.pop().unwrap();
    let release = input.pop().unwrap();
    assert_eq!(
        i32::from_le_bytes(press[8..12].try_into().unwrap()),
        INPUT_PEN_PRESS
    );
    assert_eq!(
        i32::from_le_bytes(release[8..12].try_into().unwrap()),
        INPUT_PEN_RELEASE
    );
    assert!(input.pop().is_none(), "stale pen move crossed the barrier");
}

#[test]
fn a_full_panel_queue_fails_immediately_without_poisoning_depth() {
    let (tx, _rx) = mpsc::sync_channel(1);
    tx.send(PanelCommand::Shutdown).unwrap();
    let state = HostState::new(tx, 960, 1696, 3840);
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: 44,
        generation: 3,
        epoch: 4,
        ink_enabled: false,
    });

    let started = std::time::Instant::now();
    let error = state.request_full_refresh().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(started.elapsed() < std::time::Duration::from_millis(50));
    assert_eq!(state.snapshot().queue_depth, 0);
}

#[test]
fn full_panel_queue_coalesces_damage_without_disconnect_semantics() {
    let (tx, _rx) = mpsc::sync_channel(1);
    tx.send(PanelCommand::Shutdown).unwrap();
    let state = HostState::new(tx, 960, 1696, 3840);
    let lease = crate::panel::PanelLease {
        key: 44,
        generation: 3,
        foreground_epoch: 4,
    };
    *state.foreground.lock().unwrap() = Some(ForegroundLease {
        key: lease.key,
        generation: lease.generation,
        epoch: lease.foreground_epoch,
        ink_enabled: false,
    });

    state
        .damage(
            44,
            crate::geometry::Rect::new(10, 20, 30, 40),
            RefreshIntent::Ink,
        )
        .unwrap();
    state
        .damage(
            44,
            crate::geometry::Rect::new(30, 40, 50, 60),
            RefreshIntent::Content,
        )
        .unwrap();

    let pending = state.telemetry.take_deferred_damage(lease).unwrap();
    assert_eq!(pending.rect, crate::geometry::Rect::new(10, 20, 70, 80));
    assert_eq!(pending.intent, RefreshIntent::Content);
    assert_eq!(state.snapshot().queue_depth, 0);
}
