use super::{BridgeError, LifecycleBridge};
use remagic_protocol::{read_frame, write_frame, AppCommand, Response};
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

pub(crate) struct ControlSocket {
    listener: UnixListener,
    cleanup: SocketCleanup,
}

impl ControlSocket {
    pub(crate) fn bind(path: PathBuf) -> io::Result<Self> {
        remove_stale_socket(&path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            cleanup: SocketCleanup(path),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.cleanup.0
    }

    pub(crate) async fn run(self, bridge: LifecycleBridge) -> io::Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let bridge = bridge.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_control_client(stream, bridge, 0).await {
                    eprintln!("remagic-runner: application control request failed: {error}");
                }
            });
        }
    }
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to replace non-socket control path {}",
                path.display()
            ),
        ));
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "application control socket is already live: {}",
                path.display()
            ),
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(super) async fn serve_control_client(
    mut stream: UnixStream,
    bridge: LifecycleBridge,
    required_uid: u32,
) -> Result<(), BridgeError> {
    if stream.peer_cred()?.uid() != required_uid {
        write_frame(
            &mut stream,
            &Response::Error {
                message: "permission denied".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let command: AppCommand = match read_frame(&mut stream).await {
        Ok(command) => command,
        Err(error) => {
            write_frame(
                &mut stream,
                &Response::Error {
                    message: format!("invalid application command: {error}"),
                },
            )
            .await?;
            return Ok(());
        }
    };
    let response = bridge
        .dispatch(command)
        .await
        .map(|()| Response::Ok)
        .unwrap_or_else(|error| Response::Error {
            message: error.to_string(),
        });
    write_frame(&mut stream, &response).await?;
    Ok(())
}
