//! Bounded JSONL reads from the packaged Pi RPC process.

use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

const MAX_RPC_LINE_BYTES: usize = 1024 * 1024;

pub(super) async fn next_rpc_line<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content = newline.unwrap_or(available.len());
        if bytes.len().saturating_add(content) > MAX_RPC_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Pi RPC line exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&available[..content]);
        let consumed = content + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn fragmented_crlf_and_final_unterminated_lines_are_decoded() {
        let input = b"{\"one\":1}\r\n{\"two\":2}";
        let mut reader = BufReader::with_capacity(3, &input[..]);
        assert_eq!(
            next_rpc_line(&mut reader).await.unwrap().unwrap(),
            "{\"one\":1}"
        );
        assert_eq!(
            next_rpc_line(&mut reader).await.unwrap().unwrap(),
            "{\"two\":2}"
        );
        assert!(next_rpc_line(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_pi_line_fails_before_allocating_an_unbounded_message() {
        let input = vec![b'x'; MAX_RPC_LINE_BYTES + 1];
        let mut reader = BufReader::new(&input[..]);
        let error = next_rpc_line(&mut reader).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
