//! The docker socket proxy: a byte pipe that understands just enough HTTP/1.1
//! on the client→engine side to rewrite `POST /containers/create`
//! (docs/phase5.md §2).
//!
//! `labels: { dev.rekcod.local: "node_modules,.next" }` on a container puts
//! those paths on named volumes in the VM, so installs and build output never
//! cross into macOS. Responses are never parsed. After any request with an
//! `Upgrade` header (attach, exec, BuildKit sessions) the connection is a raw
//! pipe both ways, as before.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Container label listing paths to keep on VM-local volumes (comma-separated;
/// relative paths are relative to the container's working directory).
pub const LOCAL_LABEL: &str = "dev.rekcod.local";

/// Largest create body we buffer to rewrite; bigger ones pass through untouched.
const CREATE_MAX: usize = 4 << 20;
const HEAD_MAX: usize = 64 << 10;

/// Pipes one client connection to the engine.
pub async fn pipe<C, G>(client: C, guest: G) -> Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    G: AsyncRead + AsyncWrite + Unpin,
{
    let (client_rd, mut client_wr) = tokio::io::split(client);
    let (mut guest_rd, mut guest_wr) = tokio::io::split(guest);
    let up = async {
        let mut client_rd = BufReader::new(client_rd);
        requests(&mut client_rd, &mut guest_wr).await?;
        // Upgraded (or EOF): whatever follows is opaque.
        tokio::io::copy_buf(&mut client_rd, &mut guest_wr).await?;
        guest_wr.shutdown().await?;
        anyhow::Ok(())
    };
    let down = async {
        tokio::io::copy(&mut guest_rd, &mut client_wr).await?;
        client_wr.shutdown().await?;
        anyhow::Ok(())
    };
    let (a, b) = tokio::join!(up, down);
    a.and(b)
}

/// Forwards requests until EOF or an upgrade request has been sent.
async fn requests<R, W>(rd: &mut R, wr: &mut W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let Some(head) = read_head(rd).await? else { return Ok(()) };
        let req = Head::parse(&head)?;
        if req.upgrade {
            wr.write_all(&head).await?;
            return Ok(());
        }
        let create = req.method == "POST"
            && is_create(&req.path)
            && (req.chunked || req.content_length.is_some_and(|n| n <= CREATE_MAX));
        if create {
            let body = read_body(rd, &req).await?;
            let rewritten = rewrite_create(&body, query(&req.path)).unwrap_or(body);
            wr.write_all(&req.with_length(&head, rewritten.len())).await?;
            wr.write_all(&rewritten).await?;
            continue;
        }
        wr.write_all(&head).await?;
        if req.chunked {
            copy_chunked(rd, wr).await?;
        } else if let Some(n) = req.content_length {
            let copied = tokio::io::copy(&mut (&mut *rd).take(n as u64), wr).await?;
            if copied < n as u64 {
                bail!("client closed mid-body");
            }
        }
    }
}

/// Reads up to and including the blank line; `None` on a clean EOF.
async fn read_head<R: AsyncBufRead + Unpin>(rd: &mut R) -> Result<Option<Vec<u8>>> {
    let mut head = Vec::new();
    loop {
        let n = rd.read_until(b'\n', &mut head).await?;
        if n == 0 {
            if head.is_empty() {
                return Ok(None);
            }
            bail!("client closed mid-header");
        }
        if head.ends_with(b"\r\n\r\n") || head == b"\r\n" {
            if head == b"\r\n" {
                head.clear(); // stray CRLF between requests
                continue;
            }
            return Ok(Some(head));
        }
        if head.len() > HEAD_MAX {
            bail!("request header too large");
        }
    }
}

struct Head {
    method: String,
    path: String,
    content_length: Option<usize>,
    chunked: bool,
    upgrade: bool,
}

impl Head {
    fn parse(head: &[u8]) -> Result<Head> {
        let text = std::str::from_utf8(head).context("non-UTF-8 request header")?;
        let mut lines = text.split("\r\n");
        let mut first = lines.next().unwrap_or_default().split(' ');
        let (method, path) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
        let mut h = Head { method, path, content_length: None, chunked: false, upgrade: false };
        for line in lines {
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
            match k.as_str() {
                "content-length" => h.content_length = v.parse().ok(),
                "transfer-encoding" => h.chunked = v.contains("chunked"),
                "upgrade" => h.upgrade = true,
                "connection" if v.split(',').any(|t| t.trim() == "upgrade") => h.upgrade = true,
                _ => {}
            }
        }
        Ok(h)
    }

    /// The same header block with the body length set to `len` (no chunking).
    fn with_length(&self, head: &[u8], len: usize) -> Vec<u8> {
        let text = String::from_utf8_lossy(head);
        let mut out = String::new();
        for line in text.split("\r\n").filter(|l| !l.is_empty()) {
            let key = line.split_once(':').map(|(k, _)| k.trim().to_ascii_lowercase()).unwrap_or_default();
            if key != "content-length" && key != "transfer-encoding" {
                out.push_str(line);
                out.push_str("\r\n");
            }
        }
        out.push_str(&format!("Content-Length: {len}\r\n\r\n"));
        out.into_bytes()
    }
}

fn is_create(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or_default();
    let rest = match path.strip_prefix("/v") {
        // `/v1.52/containers/create`
        Some(v) => v.split_once('/').map(|(_, r)| r).unwrap_or_default(),
        None => path.trim_start_matches('/'),
    };
    rest == "containers/create"
}

fn query(path: &str) -> &str {
    path.split_once('?').map(|(_, q)| q).unwrap_or_default()
}

async fn read_body<R: AsyncBufRead + Unpin>(rd: &mut R, req: &Head) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    if req.chunked {
        loop {
            let size = read_chunk_size(rd).await?;
            if size == 0 {
                skip_trailers(rd).await?;
                return Ok(body);
            }
            if body.len() + size > CREATE_MAX {
                bail!("create body too large");
            }
            let start = body.len();
            body.resize(start + size, 0);
            rd.read_exact(&mut body[start..]).await?;
            let mut crlf = [0u8; 2];
            rd.read_exact(&mut crlf).await?;
        }
    }
    body.resize(req.content_length.unwrap_or(0), 0);
    rd.read_exact(&mut body).await?;
    Ok(body)
}

async fn read_chunk_size<R: AsyncBufRead + Unpin>(rd: &mut R) -> Result<usize> {
    let mut line = String::new();
    if rd.read_line(&mut line).await? == 0 {
        bail!("client closed mid-chunk");
    }
    let hex = line.trim().split(';').next().unwrap_or_default();
    usize::from_str_radix(hex, 16).with_context(|| format!("bad chunk size {line:?}"))
}

async fn skip_trailers<R: AsyncBufRead + Unpin>(rd: &mut R) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line).await? == 0 || line == "\r\n" {
            return Ok(());
        }
    }
}

/// Copies a chunked body verbatim (sizes, data, trailers).
async fn copy_chunked<R, W>(rd: &mut R, wr: &mut W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).await? == 0 {
            bail!("client closed mid-chunk");
        }
        wr.write_all(line.as_bytes()).await?;
        let hex = line.trim().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(hex, 16).with_context(|| format!("bad chunk size {line:?}"))?;
        if size == 0 {
            loop {
                line.clear();
                if rd.read_line(&mut line).await? == 0 {
                    return Ok(());
                }
                wr.write_all(line.as_bytes()).await?;
                if line == "\r\n" {
                    return Ok(());
                }
            }
        }
        // data + CRLF
        let copied = tokio::io::copy(&mut (&mut *rd).take(size as u64 + 2), wr).await?;
        if copied < size as u64 + 2 {
            bail!("client closed mid-chunk");
        }
    }
}

/// Characters docker accepts in volume names; everything else becomes `-`.
fn sanitize(s: &str) -> String {
    let out: String = s
        .trim_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' })
        .collect();
    out.trim_start_matches(['-', '.', '_']).to_string()
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')).filter(|v| !v.is_empty())
}

/// Stable volume name for one VM-local path of one container:
/// `rekcod-local_<project>_<service>_<path>` for compose services, else the
/// container name, else a hash of the path.
pub fn volume_name(labels: &Value, container_name: Option<&str>, target: &str) -> String {
    let label = |k: &str| labels.get(k).and_then(Value::as_str).map(sanitize).filter(|s| !s.is_empty());
    let owner = match (label("com.docker.compose.project"), label("com.docker.compose.service")) {
        (Some(p), Some(s)) => format!("{p}_{s}"),
        _ => match container_name.map(sanitize).filter(|s| !s.is_empty()) {
            Some(n) => n,
            None => format!("{:016x}", fnv1a(target.as_bytes())),
        },
    };
    let mut name = format!("rekcod-local_{owner}_{}", sanitize(target));
    if name.len() > 200 {
        name = format!("rekcod-local_{:016x}", fnv1a(name.as_bytes()));
    }
    name
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// Normalizes `rel` against `base` (no `..` above the root).
fn resolve(base: &str, rel: &str) -> String {
    let joined = if rel.starts_with('/') { rel.to_string() } else { format!("{}/{rel}", if base.is_empty() { "/" } else { base }) };
    let mut parts: Vec<&str> = Vec::new();
    for p in joined.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The create body with VM-local volume mounts added, or `None` when the
/// container doesn't carry [`LOCAL_LABEL`] (or the body isn't JSON).
pub fn rewrite_create(body: &[u8], query: &str) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let labels = v.get("Labels").cloned().unwrap_or(Value::Null);
    let wanted = labels.get(LOCAL_LABEL)?.as_str()?.to_string();
    let workdir = v.get("WorkingDir").and_then(Value::as_str).unwrap_or_default().to_string();
    let name = query_param(query, "name");

    let host = v.as_object_mut()?.entry("HostConfig").or_insert_with(|| json!({}));
    if !host.is_object() {
        *host = json!({});
    }
    let host = host.as_object_mut()?;
    let mut taken: Vec<String> = host
        .get("Binds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| b.as_str()?.split(':').nth(1).map(|t| resolve("/", t)))
        .collect();
    let mounts = host.entry("Mounts").or_insert_with(|| json!([]));
    if !mounts.is_array() {
        *mounts = json!([]);
    }
    let mounts = mounts.as_array_mut()?;
    taken.extend(mounts.iter().filter_map(|m| m.get("Target")?.as_str().map(|t| resolve("/", t))));

    let mut changed = false;
    for path in wanted.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let target = resolve(&workdir, path);
        if target == "/" || taken.contains(&target) {
            continue;
        }
        let source = volume_name(&labels, name, &target);
        mounts.push(json!({
            "Type": "volume",
            "Source": source,
            "Target": target,
            "VolumeOptions": {"Labels": {LOCAL_LABEL: target}},
        }));
        taken.push(target);
        changed = true;
    }
    changed.then(|| serde_json::to_vec(&v).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mounts(body: &[u8]) -> Vec<(String, String)> {
        let v: Value = serde_json::from_slice(body).unwrap();
        v["HostConfig"]["Mounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["Source"].as_str().unwrap().to_string(), m["Target"].as_str().unwrap().to_string()))
            .collect()
    }

    #[test]
    fn adds_volumes_for_labelled_compose_service() {
        let body = br#"{"Image":"node:22","WorkingDir":"/app","Labels":{"dev.rekcod.local":"node_modules, .next,/cache",
            "com.docker.compose.project":"shop","com.docker.compose.service":"web"},
            "HostConfig":{"Binds":["/Users/me/shop:/app:rw"],"Mounts":[{"Type":"volume","Source":"c","Target":"/cache"}]}}"#;
        let out = rewrite_create(body, "name=shop-web-1").unwrap();
        assert_eq!(
            mounts(&out),
            vec![
                ("c".into(), "/cache".into()),
                ("rekcod-local_shop_web_app-node_modules".into(), "/app/node_modules".into()),
                ("rekcod-local_shop_web_app-.next".into(), "/app/.next".into()),
            ]
        );
    }

    #[test]
    fn untouched_without_label_and_names_without_compose() {
        assert!(rewrite_create(br#"{"Image":"x","Labels":{"a":"b"}}"#, "").is_none());
        assert!(rewrite_create(b"not json", "").is_none());
        let out = rewrite_create(br#"{"Image":"x","HostConfig":null,"Labels":{"dev.rekcod.local":"/w/../deps"}}"#, "name=api&platform=").unwrap();
        assert_eq!(mounts(&out), vec![("rekcod-local_api_deps".into(), "/deps".into())]);
        let out = rewrite_create(br#"{"Image":"x","Labels":{"dev.rekcod.local":"/d"}}"#, "").unwrap();
        assert!(mounts(&out)[0].0.starts_with("rekcod-local_"));
    }

    #[test]
    fn create_path_matching() {
        for p in ["/containers/create", "/v1.52/containers/create?name=x", "/v1.41/containers/create"] {
            assert!(is_create(p), "{p}");
        }
        for p in ["/containers/json", "/v1.52/containers/abc/start", "/containers/create/x"] {
            assert!(!is_create(p), "{p}");
        }
    }

    async fn through_proxy(input: &[u8]) -> Vec<u8> {
        let (client, mut client_far) = tokio::io::duplex(1 << 16);
        let (guest, mut guest_far) = tokio::io::duplex(1 << 16);
        let task = tokio::spawn(pipe(client, guest));
        client_far.write_all(input).await.unwrap();
        client_far.shutdown().await.unwrap();
        let mut seen = Vec::new();
        guest_far.read_to_end(&mut seen).await.unwrap();
        drop(guest_far);
        let _ = task.await;
        seen
    }

    #[tokio::test]
    async fn rewrites_create_between_other_requests() {
        let create = br#"{"Image":"x","Labels":{"dev.rekcod.local":"/d"}}"#;
        let mut input = b"GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n".to_vec();
        input.extend_from_slice(format!("POST /v1.52/containers/create?name=a HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n", create.len()).as_bytes());
        input.extend_from_slice(create);
        input.extend_from_slice(b"POST /build HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n");
        let seen = String::from_utf8(through_proxy(&input).await).unwrap();
        assert!(seen.starts_with("GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\nPOST /v1.52/containers/create?name=a"));
        assert!(seen.contains(r#""Source":"rekcod-local_a_d""#), "{seen}");
        assert!(seen.ends_with("POST /build HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"), "{seen}");
        let body_at = seen.find("{\"").unwrap();
        let len: usize = seen.split("Content-Length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
        assert_eq!(&seen[body_at..body_at + len].chars().last(), &Some('}'));
    }

    #[tokio::test]
    async fn chunked_create_and_raw_after_upgrade() {
        let create = br#"{"Image":"x","Labels":{"dev.rekcod.local":"/d"}}"#;
        let mut input = format!("POST /containers/create HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", create.len()).into_bytes();
        input.extend_from_slice(create);
        input.extend_from_slice(b"\r\n0\r\n\r\n");
        input.extend_from_slice(b"POST /containers/x/attach?stream=1 HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n");
        input.extend_from_slice(b"GET not-http \x00\x01 raw bytes");
        let seen = through_proxy(&input).await;
        let text = String::from_utf8_lossy(&seen);
        assert!(!text.contains("Transfer-Encoding"), "{text}");
        assert!(text.contains("rekcod-local_"), "{text}");
        assert!(seen.ends_with(b"Upgrade: tcp\r\n\r\nGET not-http \x00\x01 raw bytes"));
    }
}
