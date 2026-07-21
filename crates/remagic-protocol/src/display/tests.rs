use super::*;
use remagic_core::AppId;

fn token() -> AppToken {
    AppToken {
        app_id: AppId::new("magicpaper").unwrap(),
        generation: 3,
        foreground_epoch: 8,
        lease_id: Some(21),
    }
}

fn rect() -> DamageRect {
    DamageRect {
        x: 3,
        y: 4,
        width: 20,
        height: 30,
    }
}

#[test]
fn surface_stride_and_length_boundaries_are_validated() {
    let mut surface = SurfaceDescriptor {
        surface_id: 1,
        width: 954,
        height: 1696,
        stride: 954 * 4,
        byte_len: u64::from(954_u32 * 4 * 1696),
        pixel_format: PixelFormat::Xrgb8888,
    };
    assert!(surface.validate().is_ok());
    surface.stride -= 1;
    assert!(matches!(
        surface.validate(),
        Err(DisplayValidationError::InvalidStride { .. })
    ));
}

#[test]
fn damage_rect_boundary_property_holds() {
    for (rect, valid) in [
        (rect(), true),
        (DamageRect { width: 0, ..rect() }, false),
        (
            DamageRect {
                height: 0,
                ..rect()
            },
            false,
        ),
        (DamageRect { x: -1, ..rect() }, false),
        (DamageRect { y: -1, ..rect() }, false),
        (
            DamageRect {
                x: i32::MAX,
                width: 1,
                ..rect()
            },
            false,
        ),
    ] {
        assert_eq!(rect.validate().is_ok(), valid, "{rect:?}");
    }
}

#[test]
fn pen_frame_rejects_non_finite_and_out_of_range_values() {
    let baseline = PenFrame {
        sequence: 1,
        kernel_time_ns: 2,
        phase: PenPhase::Move,
        tool: PenTool::Pen,
        x: 10.0,
        y: 20.0,
        pressure: 0.5,
    };
    assert!(baseline.validate().is_ok());
    for invalid in [
        PenFrame {
            pressure: 1.1,
            ..baseline
        },
        PenFrame {
            pressure: f32::NAN,
            ..baseline
        },
        PenFrame {
            x: -0.1,
            ..baseline
        },
        PenFrame {
            y: f32::INFINITY,
            ..baseline
        },
    ] {
        assert_eq!(
            invalid.validate(),
            Err(DisplayValidationError::InvalidPenFrame)
        );
    }
}

#[test]
fn display_and_ink_messages_round_trip_with_full_fence() {
    let messages = vec![
        DisplayClientMessage::Attach {
            token: token(),
            profile: RuntimeProfile::NativeV2,
            preferred_format: PixelFormat::Xrgb8888,
        },
        DisplayClientMessage::FrameCommit {
            commit: FrameCommit {
                token: token(),
                surface_id: 2,
                frame_sequence: 15,
                damage_rects: vec![rect()],
                intent: FrameIntent::Ink,
            },
        },
        DisplayClientMessage::InkCommit {
            commit: InkCommit {
                token: token(),
                stroke_id: 90,
                frame_sequence: 16,
                damage_rects: vec![rect()],
            },
        },
        DisplayClientMessage::InkCancel {
            cancel: InkCancel {
                token: token(),
                stroke_id: Some(91),
                damage_rects: vec![rect()],
            },
        },
        DisplayClientMessage::Release { token: token() },
    ];
    for message in messages {
        assert!(message.validate().is_ok());
        let encoded = serde_json::to_vec(&message).unwrap();
        let decoded: DisplayClientMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, message);
    }

    let surface = SurfaceDescriptor {
        surface_id: 2,
        width: 954,
        height: 1696,
        stride: 954 * 4,
        byte_len: u64::from(954_u32 * 4 * 1696),
        pixel_format: PixelFormat::Xrgb8888,
    };
    let host_messages = vec![
        DisplayHostMessage::Attached {
            token: token(),
            surface,
        },
        DisplayHostMessage::PenFrame {
            token: token(),
            frame: PenFrame {
                sequence: 9,
                kernel_time_ns: 10,
                phase: PenPhase::Down,
                tool: PenTool::Pen,
                x: 11.0,
                y: 12.0,
                pressure: 0.3,
            },
        },
        DisplayHostMessage::TouchFrame {
            token: token(),
            frame: TouchFrame {
                sequence: 10,
                kernel_time_ns: 11,
                contact_id: 0,
                phase: TouchPhase::Down,
                x: 13.0,
                y: 14.0,
            },
        },
        DisplayHostMessage::LeaseRevoked {
            token: token(),
            reason: LeaseRevocationReason::Background,
        },
        DisplayHostMessage::Error {
            token: Some(token()),
            code: DisplayErrorCode::StaleToken,
            detail: "stale generation".into(),
        },
    ];
    for host in host_messages {
        let encoded = serde_json::to_vec(&host).unwrap();
        assert_eq!(
            serde_json::from_slice::<DisplayHostMessage>(&encoded).unwrap(),
            host
        );
    }
}
