use remagic_display_host::input::{InputFrame, PenFrame, PenPhase, PenTool};
use remagic_display_host::panel::{
    MemoryBackend, PanelRuntime, PanelTelemetry, RefreshIntent, SubmissionReason,
};
use remagic_display_host::protocol::{
    MESSAGE_INITIALIZE, MESSAGE_UPDATE, MESSAGE_USERINPUT, QTFB_CLIENT_MESSAGE_SIZE,
    QTFB_SERVER_MESSAGE_SIZE, UPDATE_PARTIAL,
};
use remagic_display_host::qtfb::{HostState, QtfbServer};
use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::mpsc;
use std::sync::Arc;

#[test]
fn legacy_qtfb_connect_damage_foreground_and_input_round_trip() {
    let (panel_tx, panel_rx) = mpsc::sync_channel(1024);
    let backend = MemoryBackend::new(960, 1696).unwrap();
    let telemetry = Arc::new(PanelTelemetry::default());
    let panel_telemetry = Arc::clone(&telemetry);
    let panel = std::thread::spawn(move || {
        PanelRuntime::with_telemetry(backend, panel_rx, panel_telemetry)
            .run()
            .unwrap()
    });
    let state = HostState::new_with_telemetry(panel_tx, 960, 1696, 3840, telemetry);
    let socket_path = format!("/tmp/remagic-qtfb-test-{}.sock", std::process::id());
    let server = QtfbServer::start(Arc::clone(&state), &socket_path).unwrap();
    let client = connect_seqpacket(&socket_path).unwrap();

    let mut initialize = [0_u8; QTFB_CLIENT_MESSAGE_SIZE];
    initialize[0] = MESSAGE_INITIALIZE;
    initialize[4..8].copy_from_slice(&77_i32.to_le_bytes());
    initialize[8] = 6;
    send_packet(client, &initialize).unwrap();
    let mut reply = [0_u8; QTFB_SERVER_MESSAGE_SIZE];
    recv_packet(client, &mut reply).unwrap();
    assert_eq!(reply[0], MESSAGE_INITIALIZE);
    assert!(i32::from_le_bytes(reply[8..12].try_into().unwrap()) > 0);
    assert_eq!(
        u64::from_le_bytes(reply[16..24].try_into().unwrap()),
        954 * 1696 * 2
    );

    state.set_foreground(77, 3, 9, true).unwrap();
    assert_eq!(
        state.set_foreground(77, 3, 8, false).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        state.set_foreground(77, 2, 10, false).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let mut update = [0_u8; QTFB_CLIENT_MESSAGE_SIZE];
    update[0] = MESSAGE_UPDATE;
    for (offset, value) in [
        (4, UPDATE_PARTIAL),
        (8, 10_i32),
        (12, 20_i32),
        (16, 30_i32),
        (20, 40_i32),
    ] {
        update[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    send_packet(client, &update).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let snapshot = state.snapshot();
        if snapshot.surface_sequences.get(&77).copied() == Some(1)
            && snapshot
                .surface_signatures
                .get(&77)
                .copied()
                .unwrap_or_default()
                != 0
            && snapshot.last_presented_key == Some(77)
            && snapshot.last_presented_sequence == 1
            && snapshot.panel_submission_count >= 2
            && snapshot.visible_signature != 0
        {
            assert_eq!(snapshot.full_refresh_count, 1);
            assert_eq!(
                snapshot
                    .recent_submissions
                    .iter()
                    .filter(|record| {
                        record.key == 77
                            && record.generation == 3
                            && record.foreground_epoch == 9
                            && record.intent == RefreshIntent::Full
                            && record.reason == SubmissionReason::ForegroundSwitch
                            && record.success
                    })
                    .count(),
                1
            );
            assert!(snapshot.recent_submissions.iter().any(|record| {
                record.key == 77
                    && record.generation == 3
                    && record.foreground_epoch == 9
                    && record.reason == SubmissionReason::SurfaceDamage
                    && record.success
            }));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "first frame was not presented"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    state.dispatch_input(InputFrame::Pen(PenFrame {
        sequence: 1,
        kernel_time_ns: 12,
        phase: PenPhase::Down,
        tool: PenTool::Eraser,
        x: 100,
        y: 200,
        pressure: 2048,
        pressure_max: 4096,
    }));
    let mut input = [0_u8; QTFB_SERVER_MESSAGE_SIZE];
    recv_packet(client, &mut input).unwrap();
    assert_eq!(input[0], MESSAGE_USERINPUT);
    assert_eq!(i32::from_le_bytes(input[12..16].try_into().unwrap()), 1);
    assert_eq!(i32::from_le_bytes(input[16..20].try_into().unwrap()), 100);
    assert_eq!(i32::from_le_bytes(input[24..28].try_into().unwrap()), 50);
    assert_eq!(state.snapshot().input_backpressure_events, 0);

    unsafe {
        libc::shutdown(client, libc::SHUT_RDWR);
        libc::close(client);
    }
    drop(server);
    state.shutdown();
    drop(state);
    panel.join().unwrap();
}

fn connect_seqpacket(path: &str) -> io::Result<RawFd> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = CString::new(path).unwrap();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address
        .sun_path
        .iter_mut()
        .zip(path.as_bytes_with_nul().iter().copied())
    {
        *target = source as libc::c_char;
    }
    if unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    Ok(fd)
}

fn send_packet(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let count = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) };
    if count == bytes.len() as isize {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn recv_packet(fd: RawFd, bytes: &mut [u8]) -> io::Result<()> {
    let count = unsafe { libc::recv(fd, bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if count == bytes.len() as isize {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
