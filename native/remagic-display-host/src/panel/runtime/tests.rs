use super::*;
use crate::input::{PenFrame, PenPhase, PenTool};
use crate::panel::render::{live_brush_radius, live_segment_radius, LivePenPoint};
use crate::panel::{MemoryBackend, PanelBackend, PanelCommand, PanelLease, RefreshIntent};
use crate::protocol::PixelFormat;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;

static NEXT_SHM_KEY: AtomicI32 = AtomicI32::new(100_000);

fn lease(key: i32, generation: u64, foreground_epoch: u64) -> PanelLease {
    PanelLease {
        key,
        generation,
        foreground_epoch,
    }
}

struct MockPanel {
    pixels: Vec<u8>,
    submissions: Vec<(Rect, RefreshIntent)>,
}

impl MockPanel {
    fn new() -> Self {
        Self {
            pixels: vec![0xff; 960 * 1696 * 4],
            submissions: Vec::new(),
        }
    }
}

impl PanelBackend for MockPanel {
    fn width(&self) -> i32 {
        960
    }

    fn height(&self) -> i32 {
        1696
    }

    fn stride(&self) -> usize {
        960 * 4
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    fn submit(&mut self, rect: Rect, intent: RefreshIntent) -> io::Result<u64> {
        self.submissions.push((rect, intent));
        Ok(self.submissions.len() as u64)
    }
}

fn test_surface(key: i32) -> Arc<SharedSurface> {
    loop {
        let shm_key = NEXT_SHM_KEY.fetch_add(1, Ordering::Relaxed);
        match SharedSurface::create(key, 954, 1696, PixelFormat::Rgb565, shm_key) {
            Ok(surface) => return Arc::new(surface),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("could not create test surface: {error}"),
        }
    }
}

fn memory_runtime() -> (PanelRuntime<MemoryBackend>, Arc<SharedSurface>) {
    let (_tx, rx) = mpsc::channel();
    let mut runtime = PanelRuntime::new(MemoryBackend::new(960, 1696).unwrap(), rx);
    let surface = test_surface(17);
    runtime
        .handle(PanelCommand::RegisterSurface(Arc::clone(&surface)))
        .unwrap();
    runtime
        .handle(PanelCommand::SetForeground {
            lease: lease(17, 3, 5),
            full_refresh: false,
        })
        .unwrap();
    runtime
        .handle(PanelCommand::ConfigureInk {
            lease: lease(17, 3, 5),
            enabled: true,
            region: None,
        })
        .unwrap();
    runtime.backend.clear_submissions();
    (runtime, surface)
}

fn pen_frame(
    sequence: u64,
    phase: PenPhase,
    tool: PenTool,
    x: i32,
    y: i32,
    pressure: i32,
) -> PenFrame {
    PenFrame {
        sequence,
        kernel_time_ns: 0,
        phase,
        tool,
        x,
        y,
        pressure,
        pressure_max: 4096,
    }
}

fn physical_pixel(runtime: &PanelRuntime<MemoryBackend>, x: i32, y: i32) -> [u8; 4] {
    let (x, y) = Geometry::new(954, 1696, 960, 1696)
        .unwrap()
        .logical_to_physical_point(x, y);
    let index = y as usize * runtime.backend.stride() + x as usize * 4;
    runtime.backend.pixels()[index..index + 4]
        .try_into()
        .unwrap()
}

#[test]
fn stale_ink_lease_is_rejected() {
    let (_tx, rx) = mpsc::channel();
    let mut runtime = PanelRuntime::new(MockPanel::new(), rx);
    runtime.foreground = Some(lease(1, 2, 3));
    let error = runtime
        .handle(PanelCommand::ConfigureInk {
            lease: lease(1, 1, 3),
            enabled: true,
            region: None,
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn live_ink_draws_a_tight_dirty_rect() {
    let (_tx, rx) = mpsc::channel();
    let mut runtime = PanelRuntime::new(MockPanel::new(), rx);
    runtime.foreground = Some(lease(1, 2, 3));
    runtime.ink = InkLease {
        key: 1,
        generation: 2,
        epoch: 3,
        enabled: true,
        region: None,
    };
    runtime
        .handle_pen(
            lease(1, 2, 3),
            pen_frame(1, PenPhase::Down, PenTool::Pen, 100, 200, 2000),
        )
        .unwrap();
    runtime.flush_live(true).unwrap();
    assert_eq!(runtime.backend.submissions.len(), 1);
    let (dirty, intent) = runtime.backend.submissions[0];
    assert_eq!(intent, RefreshIntent::Ink);
    assert!(dirty.width < 20 && dirty.height < 20);
}

#[test]
fn up_without_a_new_canonical_commit_never_overwrites_live_ink() {
    let (mut runtime, _surface) = memory_runtime();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(1, PenPhase::Down, PenTool::Pen, 100, 200, 2048),
        )
        .unwrap();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(2, PenPhase::Move, PenTool::Pen, 120, 200, 4096),
        )
        .unwrap();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(3, PenPhase::Up, PenTool::Pen, 120, 200, 0),
        )
        .unwrap();

    assert_eq!(runtime.backend.submissions().len(), 1);
    assert_eq!(runtime.backend.submissions()[0].intent, RefreshIntent::Ink);
    assert_eq!(physical_pixel(&runtime, 110, 200), [0, 0, 0, 0xff]);

    let now = Instant::now();
    runtime.settle_started = Some(now - CANONICAL_SETTLE_LIMIT);
    runtime.settle_deadline = Some(now);
    runtime.flush_deadlines().unwrap();

    assert!(runtime.settle_deadline.is_none());
    assert_eq!(runtime.backend.submissions().len(), 1);
    assert_eq!(physical_pixel(&runtime, 110, 200), [0, 0, 0, 0xff]);
}

#[test]
fn new_commit_settles_in_memory_without_an_extra_panel_submission() {
    let (mut runtime, surface) = memory_runtime();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(1, PenPhase::Down, PenTool::Pen, 100, 200, 1024),
        )
        .unwrap();
    runtime.flush_live(true).unwrap();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(2, PenPhase::Move, PenTool::Pen, 130, 200, 4096),
        )
        .unwrap();
    runtime.flush_live(true).unwrap();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(3, PenPhase::Up, PenTool::Pen, 130, 200, 0),
        )
        .unwrap();
    assert_eq!(
        runtime
            .backend
            .submissions()
            .iter()
            .map(|submission| submission.intent)
            .collect::<Vec<_>>(),
        vec![RefreshIntent::Ink, RefreshIntent::Ink]
    );
    assert_eq!(physical_pixel(&runtime, 115, 200), [0, 0, 0, 0xff]);

    surface.mark_commit();
    runtime
        .handle(PanelCommand::Damage {
            lease: lease(17, 3, 5),
            rect: Rect::new(90, 185, 55, 30),
            intent: RefreshIntent::Ink,
        })
        .unwrap();
    assert_eq!(runtime.backend.submissions().len(), 2);

    runtime.settle_deadline = Some(Instant::now());
    runtime.flush_deadlines().unwrap();
    assert_eq!(
        runtime
            .backend
            .submissions()
            .iter()
            .map(|submission| submission.intent)
            .collect::<Vec<_>>(),
        vec![RefreshIntent::Ink, RefreshIntent::Ink]
    );
    assert_eq!(physical_pixel(&runtime, 115, 200), [0xff; 4]);

    runtime.flush_deadlines().unwrap();
    assert_eq!(runtime.backend.submissions().len(), 2);
}

#[test]
fn memory_backend_brush_matches_magicpaper_pressure_and_eraser_radii() {
    assert_eq!(live_brush_radius(PenTool::Pen, 0, 4096), 2);
    assert_eq!(live_brush_radius(PenTool::Pen, 2048, 4096), 3);
    assert_eq!(live_brush_radius(PenTool::Pen, 4096, 4096), 5);
    assert_eq!(live_brush_radius(PenTool::Eraser, 0, 4096), 22);
    assert_eq!(live_brush_radius(PenTool::Eraser, 4096, 4096), 22);
    let previous = LivePenPoint {
        x: 0,
        y: 0,
        radius: 2,
        tool: PenTool::Pen,
    };
    assert_eq!(live_segment_radius(PenTool::Pen, 5, Some(previous)), 3);

    let (mut runtime, _surface) = memory_runtime();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(1, PenPhase::Down, PenTool::Eraser, 400, 500, 0),
        )
        .unwrap();
    runtime
        .handle_pen(
            lease(17, 3, 5),
            pen_frame(2, PenPhase::Up, PenTool::Eraser, 400, 500, 0),
        )
        .unwrap();
    assert_eq!(runtime.backend.submissions().len(), 1);
    assert_eq!(runtime.backend.submissions()[0].rect.width, 45);
    assert_eq!(runtime.backend.submissions()[0].rect.height, 45);
}

#[test]
fn stale_foreground_commands_cannot_touch_the_new_lease() {
    let (_tx, rx) = mpsc::channel();
    let telemetry = Arc::new(PanelTelemetry::default());
    let mut runtime = PanelRuntime::with_telemetry(
        MemoryBackend::new(960, 1696).unwrap(),
        rx,
        Arc::clone(&telemetry),
    );
    let first = test_surface(31);
    let second = test_surface(32);
    for surface in [&first, &second] {
        runtime
            .handle(PanelCommand::RegisterSurface(Arc::clone(surface)))
            .unwrap();
    }
    let first_lease = lease(31, 1, 10);
    let second_lease = lease(32, 2, 20);
    runtime
        .handle(PanelCommand::SetForeground {
            lease: first_lease,
            full_refresh: true,
        })
        .unwrap();
    runtime
        .handle(PanelCommand::ConfigureInk {
            lease: first_lease,
            enabled: true,
            region: None,
        })
        .unwrap();
    runtime
        .handle(PanelCommand::SetForeground {
            lease: second_lease,
            full_refresh: true,
        })
        .unwrap();
    let baseline = runtime.backend.submissions().len();

    for command in [
        PanelCommand::Damage {
            lease: first_lease,
            rect: Rect::new(0, 0, 40, 40),
            intent: RefreshIntent::Ui,
        },
        PanelCommand::FullRefresh { lease: first_lease },
        PanelCommand::Pen {
            lease: first_lease,
            frame: pen_frame(1, PenPhase::Down, PenTool::Pen, 10, 10, 4096),
        },
        PanelCommand::ClearForeground { lease: first_lease },
    ] {
        runtime.handle(command).unwrap();
    }
    assert_eq!(runtime.backend.submissions().len(), baseline);
    assert_eq!(runtime.foreground, Some(second_lease));

    let switch_full = telemetry
        .recent_submissions()
        .into_iter()
        .filter(|record| {
            record.key == second_lease.key
                && record.generation == second_lease.generation
                && record.foreground_epoch == second_lease.foreground_epoch
                && record.intent == RefreshIntent::Full
                && record.reason == SubmissionReason::ForegroundSwitch
                && record.success
        })
        .count();
    assert_eq!(switch_full, 1);
}

#[test]
fn submission_evidence_is_bounded_and_keeps_the_exact_fence() {
    let (mut runtime, _surface) = memory_runtime();
    for _ in 0..80 {
        runtime
            .handle(PanelCommand::FullRefresh {
                lease: lease(17, 3, 5),
            })
            .unwrap();
    }
    let records = runtime.telemetry.recent_submissions();
    assert_eq!(records.len(), crate::panel::SUBMISSION_HISTORY_CAPACITY);
    assert!(records
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(records.iter().all(|record| {
        record.key == 17
            && record.generation == 3
            && record.foreground_epoch == 5
            && record.marker.is_some()
            && record.success
    }));
}

#[test]
fn panel_queue_depth_counts_every_enqueue_and_dequeue_including_shutdown() {
    let (tx, rx) = mpsc::channel();
    let telemetry = Arc::new(PanelTelemetry::default());
    let runtime = PanelRuntime::with_telemetry(
        MemoryBackend::new(32, 32).unwrap(),
        rx,
        Arc::clone(&telemetry),
    );
    for (expected, command) in [
        (
            1,
            PanelCommand::FullRefresh {
                lease: lease(1, 1, 1),
            },
        ),
        (
            2,
            PanelCommand::ClearForeground {
                lease: lease(1, 1, 1),
            },
        ),
        (3, PanelCommand::Shutdown),
    ] {
        telemetry.command_enqueued();
        tx.send(command).unwrap();
        assert_eq!(telemetry.queue_depth(), expected);
    }
    drop(tx);
    runtime.run().unwrap();
    assert_eq!(telemetry.queue_depth(), 0);
    telemetry.command_dequeued();
    assert_eq!(telemetry.queue_depth(), 0);
}
