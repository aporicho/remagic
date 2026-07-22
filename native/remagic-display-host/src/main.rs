use remagic_device::{DeviceProduct, DeviceProfile};
use remagic_display_host::control::ControlServer;
use remagic_display_host::input::{CapturedInput, InputThreads};
#[cfg(feature = "device")]
use remagic_display_host::panel::QuillBackend;
use remagic_display_host::panel::{
    MemoryBackend, PanelBackend, PanelCommand, PanelRuntime, PanelTelemetry,
};
use remagic_display_host::protocol::{DISPLAY_CONTROL_SOCKET, QTFB_SOCKET};
use remagic_display_host::qtfb::{HostState, QtfbServer};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "--check" {
        println!(
            "{{\"ok\":true,\"protocol\":1,\"qtfb\":\"{}\",\"control\":\"{}\",\"device_backend\":{}}}",
            QTFB_SOCKET,
            DISPLAY_CONTROL_SOCKET,
            cfg!(feature = "device")
        );
        return Ok(());
    }

    if mode == "--mock" {
        let profile = DeviceProfile::for_product(DeviceProduct::PaperProMove, "mock");
        return run(|| MemoryBackend::new(960, 1696), false, profile).map_err(Into::into);
    }

    #[cfg(feature = "device")]
    {
        verify_exclusive_domain()?;
        let profile = DeviceProfile::detect()?;
        run(QuillBackend::open, true, profile).map_err(Into::into)
    }
    #[cfg(not(feature = "device"))]
    {
        Err("this build has no device backend; use --mock or rebuild with --features device".into())
    }
}

#[cfg(feature = "device")]
fn verify_exclusive_domain() -> Result<(), Box<dyn std::error::Error>> {
    if !std::path::Path::new("/run/remagic/managed-domain").is_file() {
        return Err("managed-domain marker is absent".into());
    }
    for unit in [
        "xochitl.service",
        "paperweight.service",
        "remagic-runtime.service",
    ] {
        let active = std::process::Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .is_ok_and(|status| status.success());
        if active {
            return Err(format!("refusing display ownership while {unit} is active").into());
        }
    }
    Ok(())
}

fn run<B, F>(backend_factory: F, claim_input: bool, profile: DeviceProfile) -> io::Result<()>
where
    B: PanelBackend + 'static,
    F: FnOnce() -> io::Result<B> + Send + 'static,
{
    profile.validate().map_err(io::Error::other)?;
    let panel = spawn_panel_worker(
        backend_factory,
        profile.display.logical_width,
        profile.display.logical_height,
    )?;
    let health = Arc::clone(&panel.telemetry);
    let state = HostState::new_with_telemetry(
        panel.sender,
        panel.width,
        panel.height,
        panel.stride,
        panel.telemetry,
    );

    let qtfb_path = std::env::var("REMAGIC_QTFB_SOCKET").unwrap_or_else(|_| QTFB_SOCKET.into());
    let control_path =
        std::env::var("REMAGIC_DISPLAY_SOCKET").unwrap_or_else(|_| DISPLAY_CONTROL_SOCKET.into());
    let qtfb = QtfbServer::start(Arc::clone(&state), &qtfb_path)?;
    let control = ControlServer::start(Arc::clone(&state), control_path.clone())?;

    let (input_tx, input_rx) = mpsc::channel::<CapturedInput>();
    let input_threads = if claim_input {
        Some(InputThreads::spawn(
            profile.display.logical_width,
            profile.display.logical_height,
            input_tx,
            state.input_epoch_source(),
        )?)
    } else {
        None
    };
    let input_state = Arc::clone(&state);
    let input_dispatch = std::thread::Builder::new()
        .name("remagic-input-dispatch".into())
        .spawn(move || {
            while let Ok(captured) = input_rx.recv() {
                let _ = input_state.dispatch_captured_input(captured);
                if input_state.is_shutdown() {
                    break;
                }
            }
        })?;

    let terminating = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&terminating))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&terminating))?;
    eprintln!(
        "remagic-display-host: ready product={:?} physical={}x{} logical={}x{} stride={} qtfb={} control={}",
        profile.product,
        panel.width,
        panel.height,
        profile.display.logical_width,
        profile.display.logical_height,
        panel.stride,
        qtfb_path,
        control_path
    );
    let mut hardware_failure = None;
    while !terminating.load(Ordering::Acquire) && !state.is_shutdown() {
        let (_, _, panel_failures, _) = health.snapshot();
        if panel_failures > 0 {
            hardware_failure = Some(format!(
                "panel worker reported {panel_failures} hardware submission failure(s)"
            ));
            break;
        }
        if input_threads.as_ref().is_some_and(InputThreads::failed) {
            hardware_failure = Some("marker or touch input worker stopped unexpectedly".into());
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    state.shutdown();
    drop(input_threads);
    drop(control);
    drop(qtfb);
    drop(state);
    let _ = input_dispatch.join();
    panel
        .thread
        .join()
        .map_err(|_| io::Error::other("panel thread panicked"))??;
    match hardware_failure {
        Some(error) => Err(io::Error::other(error)),
        None => Ok(()),
    }
}

struct PanelWorker {
    sender: mpsc::SyncSender<PanelCommand>,
    telemetry: Arc<PanelTelemetry>,
    width: i32,
    height: i32,
    stride: usize,
    thread: std::thread::JoinHandle<io::Result<()>>,
}

fn spawn_panel_worker<B, F>(
    backend_factory: F,
    logical_width: i32,
    logical_height: i32,
) -> io::Result<PanelWorker>
where
    B: PanelBackend + 'static,
    F: FnOnce() -> io::Result<B> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1024);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let telemetry = Arc::new(PanelTelemetry::default());
    let panel_telemetry = Arc::clone(&telemetry);
    let panel_health = Arc::clone(&telemetry);
    let thread = std::thread::Builder::new()
        .name("remagic-panel".into())
        .spawn(move || {
            // Quill initializes QCoreApplication and vendor-owned Qt objects.
            // This worker therefore owns init, swaps, processEvents, and drop.
            let backend = backend_factory().inspect_err(|error| {
                let _ = ready_tx.send(Err(error.to_string()));
            })?;
            let dimensions = validate_backend(&backend, logical_width, logical_height);
            if let Err(error) = &dimensions {
                let _ = ready_tx.send(Err(error.to_string()));
                panel_health.mark_failure();
                return Err(io::Error::other(error.to_string()));
            }
            if ready_tx.send(dimensions).is_err() {
                return Ok(());
            }
            let result = PanelRuntime::with_telemetry(backend, receiver, panel_telemetry).run();
            if result.is_err() {
                panel_health.mark_failure();
            }
            result
        })?;
    let (width, height, stride) = ready_rx
        .recv()
        .map_err(|_| io::Error::other("panel worker stopped during initialization"))?
        .map_err(io::Error::other)?;
    Ok(PanelWorker {
        sender,
        telemetry,
        width,
        height,
        stride,
        thread,
    })
}

fn validate_backend<B: PanelBackend>(
    backend: &B,
    logical_width: i32,
    logical_height: i32,
) -> Result<(i32, i32, usize), String> {
    let dimensions = (backend.width(), backend.height(), backend.stride());
    if dimensions.0 < logical_width
        || dimensions.1 < logical_height
        || dimensions.2 < dimensions.0 as usize * 4
    {
        Err(format!(
            "unsupported physical framebuffer {}x{} stride {}",
            dimensions.0, dimensions.1, dimensions.2
        ))
    } else {
        Ok(dimensions)
    }
}
