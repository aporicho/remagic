use super::*;
use crate::bootstrap::lifecycle_socket_pair;
use remagic_core::AppId;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::time::Duration;

#[tokio::test]
async fn lifecycle_descriptor_is_inherited_and_bidirectional() {
    let (parent, child) = lifecycle_socket_pair().unwrap();
    let child_fd = child.as_raw_fd();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(
            "fd=$REMAGIC_LIFECYCLE_FD; \
             eval \"exec 8>&$fd\"; eval \"exec 9<&$fd\"; \
             command=$(dd bs=64 count=1 <&9 2>/dev/null); \
             printf 'child:%s' \"$command\" >&8",
        )
        .env("REMAGIC_LIFECYCLE_FD", child_fd.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || clear_close_on_exec(child_fd));
    }
    let mut process = command.spawn().unwrap();
    drop(child);

    let sent = unsafe {
        libc::send(
            parent.as_raw_fd(),
            b"runner-command\n".as_ptr().cast(),
            b"runner-command\n".len(),
            libc::MSG_NOSIGNAL,
        )
    };
    assert_eq!(sent, b"runner-command\n".len() as isize);

    let mut buffer = [0_u8; 64];
    let received = loop {
        let received = unsafe {
            libc::recv(
                parent.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            )
        };
        if received >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
            break received;
        }
        tokio::task::yield_now().await;
    };
    assert!(received > 0, "{}", io::Error::last_os_error());
    assert_eq!(&buffer[..received as usize], b"child:runner-command");
    assert!(process.wait().await.unwrap().success());
}

#[tokio::test]
async fn blocked_lifecycle_delivery_cannot_bypass_term_and_kill_deadlines() {
    let (parent, unread_peer) = lifecycle_socket_pair().unwrap();
    let payload = [0_u8; 4096];
    loop {
        let sent = unsafe {
            libc::send(
                parent.as_raw_fd(),
                payload.as_ptr().cast(),
                payload.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent >= 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        break;
    }

    let bridge =
        LifecycleBridge::new(parent, AppId::new("magicpaper").unwrap(), 1, 1, 1, 300).unwrap();
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("trap '' TERM; exec sleep 60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let policy = ShutdownPolicy {
        graceful_timeout_ms: 100,
        term_timeout_ms: 200,
        kill_timeout_ms: 350,
    };
    let started = Instant::now();
    let status = graceful_stop(&mut child, Some(&bridge), &policy)
        .await
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(unread_peer);
}
