//! Just enough HTTP to talk to the Docker API over a raw stream.
//!
//! Requests are HTTP/1.0, so dockerd answers without chunked encoding and
//! closes the connection when done; streaming endpoints (`/events`) send raw
//! newline-delimited JSON.

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest response [`get`] reads; a large `/containers/json` is a few MiB. The other end
/// may be a sandbox's guest, which decides how much it sends.
const BODY_MAX: u64 = 64 << 20;
/// Longest header block, and longest event line, read from a stream.
const HEAD_MAX: usize = 64 << 10;
const LINE_MAX: usize = 1 << 20;

/// GET `path`; the body of a 200 answer. Callers put a deadline on it.
pub async fn get<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, path: &str) -> Result<Vec<u8>> {
    send_get(&mut stream, path).await?;
    let mut buf = Vec::new();
    (&mut stream).take(BODY_MAX + 1).read_to_end(&mut buf).await?;
    if buf.len() as u64 > BODY_MAX {
        bail!("GET {path}: response over {} MiB", BODY_MAX >> 20);
    }
    let (status, body_at) = parse_head(&buf)?.context("incomplete HTTP response")?;
    if status != 200 {
        bail!("GET {path}: HTTP {status}: {}", String::from_utf8_lossy(&buf[body_at..]).trim());
    }
    Ok(buf[body_at..].to_vec())
}

/// A streaming response read as newline-delimited lines (`/events`).
pub struct Lines<S> {
    stream: S,
    buf: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Lines<S> {
    pub async fn open(mut stream: S, path: &str) -> Result<Lines<S>> {
        send_get(&mut stream, path).await?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                bail!("GET {path}: connection closed before headers");
            }
            buf.extend_from_slice(&chunk[..n]);
            match parse_head(&buf)? {
                Some((200, at)) => {
                    buf.drain(..at);
                    return Ok(Lines { stream, buf });
                }
                Some((status, _)) => bail!("GET {path}: HTTP {status}"),
                None if buf.len() > HEAD_MAX => bail!("GET {path}: header over {} KiB", HEAD_MAX >> 10),
                None => {}
            }
        }
    }

    /// Next line without its newline; `None` when the server closes.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
                line.pop();
                return Ok(Some(line));
            }
            if self.buf.len() > LINE_MAX {
                bail!("a line over {} MiB", LINE_MAX >> 20);
            }
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

async fn send_get<S: AsyncWrite + Unpin>(stream: &mut S, path: &str) -> Result<()> {
    stream.write_all(format!("GET {path} HTTP/1.0\r\nHost: docker\r\n\r\n").as_bytes()).await?;
    Ok(())
}

/// Returns `(status, body offset)` once the full header block has arrived.
pub fn parse_head(buf: &[u8]) -> Result<Option<(u16, usize)>> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..end]).context("non-UTF-8 HTTP header")?;
    let status_line = head.lines().next().unwrap_or_default();
    let status =
        status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).with_context(|| format!("bad status line {status_line:?}"))?;
    Ok(Some((status, end + 4)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_body_offset() {
        let raw = b"HTTP/1.0 200 OK\r\nApi-Version: 1.52\r\n\r\nOK";
        let (status, at) = parse_head(raw).unwrap().unwrap();
        assert_eq!((status, &raw[at..]), (200, &b"OK"[..]));
        assert_eq!(parse_head(b"HTTP/1.0 200 OK\r\nX: y\r\n").unwrap(), None);
        assert!(parse_head(b"garbage\r\n\r\n").is_err());
    }

    #[tokio::test]
    async fn replies_have_a_size_limit() {
        let (mut server, client) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let mut req = [0u8; 256];
            let _ = server.read(&mut req).await;
            server.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await.unwrap();
            let junk = vec![b'x'; 1 << 20];
            while server.write_all(&junk).await.is_ok() {}
        });
        let err = get(client, "/containers/json").await.unwrap_err();
        assert!(err.to_string().contains("over 64 MiB"), "{err}");

        let (mut server, client) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let mut req = [0u8; 256];
            let _ = server.read(&mut req).await;
            server.write_all(b"HTTP/1.0 200 OK\r\n\r\n{}\n").await.unwrap();
            let junk = vec![b'x'; 1 << 16];
            while server.write_all(&junk).await.is_ok() {}
        });
        let mut events = Lines::open(client, "/events").await.unwrap();
        assert_eq!(events.next().await.unwrap(), Some(b"{}".to_vec()));
        assert!(events.next().await.is_err(), "a line that never ends");
    }

    #[tokio::test]
    async fn streams_lines_split_across_reads() {
        let (mut server, client) = tokio::io::duplex(64);
        tokio::spawn(async move {
            let mut req = [0u8; 256];
            let _ = server.read(&mut req).await;
            for part in [&b"HTTP/1.0 200 OK\r\n\r\n{\"a\""[..], b":1}\n{\"b\":", b"2}\n"] {
                server.write_all(part).await.unwrap();
            }
        });
        let mut stream = Lines::open(client, "/events").await.unwrap();
        let mut lines = Vec::new();
        while let Some(line) = stream.next().await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]);
    }

    /// A server that answers 200 and then sends `n` bytes of `fill`.
    fn endless(fill: u8, n: usize) -> tokio::io::DuplexStream {
        let (mut server, client) = tokio::io::duplex(64 << 10);
        tokio::spawn(async move {
            let mut req = [0u8; 256];
            let _ = server.read(&mut req).await;
            server.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await.unwrap();
            let chunk = vec![fill; 64 << 10];
            let mut left = n;
            while left > 0 && server.write_all(&chunk[..left.min(chunk.len())]).await.is_ok() {
                left = left.saturating_sub(chunk.len());
            }
        });
        client
    }

    #[tokio::test]
    async fn a_guest_cant_send_without_end() {
        let mut lines = Lines::open(endless(b'x', 2 << 20), "/events").await.unwrap();
        assert!(lines.next().await.is_err());
        assert!(get(endless(b'x', BODY_MAX as usize + (1 << 20)), "/containers/json").await.is_err());
    }
}
