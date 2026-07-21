use super::*;

fn event(event_type: u16, code: u16, value: i32) -> RawEvent {
    RawEvent {
        time_ns: 123,
        event_type,
        code,
        value,
    }
}

#[test]
fn marker_emits_complete_down_move_up_frames() {
    let mut decoder = MarkerDecoder::new(
        954,
        1696,
        AxisRange {
            minimum: 0,
            maximum: 6760,
        },
        AxisRange {
            minimum: 0,
            maximum: 11960,
        },
        AxisRange {
            minimum: 0,
            maximum: 4096,
        },
    );
    decoder.consume(event(EV_ABS, ABS_X, 3380));
    decoder.consume(event(EV_ABS, ABS_Y, 5980));
    decoder.consume(event(EV_ABS, ABS_PRESSURE, 2000));
    decoder.consume(event(EV_KEY, BTN_TOUCH, 1));
    let down = decoder.consume(event(EV_SYN, SYN_REPORT, 0)).unwrap();
    assert_eq!(down.phase, PenPhase::Down);
    assert!((down.x - 477).abs() <= 1);
    assert!((down.y - 848).abs() <= 1);

    decoder.consume(event(EV_ABS, ABS_X, 4000));
    assert_eq!(
        decoder.consume(event(EV_SYN, SYN_REPORT, 0)).unwrap().phase,
        PenPhase::Move
    );
    decoder.consume(event(EV_KEY, BTN_TOUCH, 0));
    assert_eq!(
        decoder.consume(event(EV_SYN, SYN_REPORT, 0)).unwrap().phase,
        PenPhase::Up
    );
}

#[test]
fn marker_syn_dropped_cancels_active_stroke() {
    let range = AxisRange {
        minimum: 0,
        maximum: 100,
    };
    let mut decoder = MarkerDecoder::new(100, 100, range, range, range);
    decoder.consume(event(EV_KEY, BTN_TOUCH, 1));
    decoder.consume(event(EV_SYN, SYN_REPORT, 0)).unwrap();
    assert_eq!(
        decoder
            .consume(event(EV_SYN, SYN_DROPPED, 0))
            .unwrap()
            .phase,
        PenPhase::Cancel
    );
}

#[test]
fn touch_slots_keep_identity_until_release() {
    let range = AxisRange {
        minimum: 0,
        maximum: 100,
    };
    let mut decoder = TouchDecoder::new(100, 100, range, range, 2);
    decoder.consume(event(EV_ABS, ABS_MT_TRACKING_ID, 42));
    decoder.consume(event(EV_ABS, ABS_MT_POSITION_X, 10));
    decoder.consume(event(EV_ABS, ABS_MT_POSITION_Y, 20));
    let frames = decoder.consume(event(EV_SYN, SYN_REPORT, 0));
    assert_eq!(frames[0].phase, TouchPhase::Down);
    assert_eq!(frames[0].device_id, 42);
    decoder.consume(event(EV_ABS, ABS_MT_TRACKING_ID, -1));
    let frames = decoder.consume(event(EV_SYN, SYN_REPORT, 0));
    assert_eq!(frames[0].phase, TouchPhase::Up);
    assert_eq!(frames[0].device_id, 42);
}
