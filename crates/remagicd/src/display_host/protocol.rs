use super::{Snapshot, SOCKET};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MAX_REPLY_BYTES: u64 = 64 * 1024;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
pub(super) struct Reply {
    protocol: u32,
    request_id: String,
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    pub(super) snapshot: Option<Snapshot>,
}

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    protocol: u32,
    request_id: String,
    #[serde(flatten)]
    command: &'a T,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(super) enum Command {
    Status,
    SetForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        full_refresh: bool,
    },
    PrepareForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
    },
    ActivateForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        ink_enabled: bool,
        full_refresh: bool,
    },
    ClearForeground,
    ConfigureInk {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        enabled: bool,
    },
    ShowLock {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        sleep_epoch: u64,
    },
    CancelLock {
        sleep_epoch: u64,
        replacement_surface_sequence: u64,
    },
}

pub(super) async fn request(command: &Command) -> Result<Reply, String> {
    let request_id = format!(
        "remagicd-{}-{}",
        std::process::id(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    let envelope = Envelope {
        protocol: 1,
        request_id: request_id.clone(),
        command,
    };
    let mut bytes = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let operation = async {
        let mut stream = UnixStream::connect(SOCKET)
            .await
            .map_err(|error| format!("cannot connect to display host: {error}"))?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|error| format!("cannot write display command: {error}"))?;
        stream
            .shutdown()
            .await
            .map_err(|error| format!("cannot finish display command: {error}"))?;
        let mut line = String::new();
        let mut reader = BufReader::new(stream).take(MAX_REPLY_BYTES);
        let count = reader
            .read_line(&mut line)
            .await
            .map_err(|error| format!("cannot read display reply: {error}"))?;
        if count == 0 || !line.ends_with('\n') {
            return Err("display host closed without a complete reply".into());
        }
        let reply: Reply = serde_json::from_str(&line)
            .map_err(|error| format!("invalid display reply: {error}"))?;
        if reply.protocol != 1 || reply.request_id != request_id {
            return Err(format!(
                "display host reply identity mismatch: protocol={} request_id={}",
                reply.protocol, reply.request_id
            ));
        }
        if reply.ok {
            Ok(reply)
        } else {
            Err(reply
                .error
                .unwrap_or_else(|| "display host rejected the command".into()))
        }
    };
    tokio::time::timeout(Duration::from_secs(3), operation)
        .await
        .map_err(|_| "display host command timed out".to_string())?
}
