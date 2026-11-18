//! A small S3 client for RustFS (docs/rustfs.md): SigV4, path-style URLs, HTTP/1.1 over
//! plain TCP or TLS (`https://` endpoints are checked against the Mac's trusted roots).
//! Covers what rekcod needs: buckets, objects streamed from and to files, presigned GETs,
//! and RustFS's MinIO-style admin API for per-app users and policies.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use hmac::Mac;
use sha2::{Digest, Sha256};

/// RustFS answers to any region; this is what requests are signed for unless one is set.
pub const REGION: &str = "us-east-1";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct S3 {
    /// `http://host:port` or `https://host[:port]`.
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    /// The signing region (`AWS_REGION`); [`REGION`] when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

pub enum Body<'a> {
    Empty,
    Bytes(&'a [u8]),
    /// Sent unsigned (`UNSIGNED-PAYLOAD`), so a big file isn't read twice.
    File(&'a Path),
}

pub struct Response {
    pub status: u16,
    body: Box<dyn Read + Send>,
}

impl Response {
    pub fn text(mut self) -> Result<String> {
        let mut s = String::new();
        self.body.read_to_string(&mut s)?;
        Ok(s)
    }

    /// Fails with the server's `<Message>` unless the status is 2xx.
    fn ok(self, what: &str) -> Result<Response> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        let status = self.status;
        let text = self.text().unwrap_or_default();
        let msg = xml_values(&text, "Message").into_iter().next().or_else(|| xml_values(&text, "Code").into_iter().next());
        bail!("{what}: {} ({status})", msg.unwrap_or_else(|| text.trim().chars().take(200).collect()))
    }
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn hmac(key: &[u8], msg: &str) -> Vec<u8> {
    let mut m = hmac::Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// SigV4's URI encoding: everything but unreserved characters; `/` too unless it's a path.
pub fn uri_encode(s: &str, path: bool) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b'/' if path => "/".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `YYYYMMDDTHHMMSSZ` for a Unix time.
pub fn amz_date(t: u64) -> String {
    let (days, secs) = (t / 86400, t % 86400);
    // Howard Hinnant's days-to-civil.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn canonical_query(query: &[(String, String)]) -> String {
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    q.sort();
    q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// The signature for a canonical request; `headers` are lowercase and sorted.
#[allow(clippy::too_many_arguments)]
fn signature(
    secret: &str,
    region: &str,
    date: &str,
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    payload: &str,
) -> String {
    let canon_headers: String = headers.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let creq = format!("{method}\n{path}\n{query}\n{canon_headers}\n{}\n{payload}", signed.join(";"));
    let day = &date[..8];
    let scope = format!("{day}/{region}/s3/aws4_request");
    let sts = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", sha256_hex(creq.as_bytes()));
    let mut k = hmac(format!("AWS4{secret}").as_bytes(), day);
    for part in [region, "s3", "aws4_request"] {
        k = hmac(&k, part);
    }
    hex(&hmac(&k, &sts))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `<Tag>...</Tag>` contents as they appear in the document (markup and entities included), in order.
fn xml_raw<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(&after[..j]);
        rest = &after[j + close.len()..];
    }
    out
}

/// `<Tag>value</Tag>` text, decoded, in order. Enough for ListObjectsV2 and error bodies.
pub fn xml_values(xml: &str, tag: &str) -> Vec<String> {
    xml_raw(xml, tag).into_iter().map(unescape).collect()
}

/// Decodes XML entities in one pass, so `&amp;lt;` is `&lt;`, not `<`. Unknown or invalid
/// references are left as they are.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').and_then(|j| {
            let c = match &rest[1..j] {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                e => e
                    .strip_prefix("#x")
                    .map(|h| u32::from_str_radix(h, 16))
                    .or_else(|| e.strip_prefix('#').map(str::parse::<u32>))
                    .and_then(Result::ok)
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, j))
        });
        match decoded {
            Some((c, j)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn early_eof(what: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, what)
}

/// A `Content-Length` body. A connection that closes before the last byte is an
/// `UnexpectedEof` error, never a short object.
struct Exact<R> {
    r: R,
    left: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let max = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.r.read(&mut buf[..max])?;
        if n == 0 {
            return Err(early_eof(format!("connection closed with {} bytes of the body still to come", self.left)));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// Reads a `Transfer-Encoding: chunked` body. It ends only at the zero-size chunk; a
/// connection that closes before it is an `UnexpectedEof` error.
struct Chunked<R> {
    r: R,
    left: usize,
    /// A chunk's data was read; its CRLF comes next.
    crlf: bool,
    done: bool,
}

impl<R> Chunked<R> {
    fn new(r: R) -> Chunked<R> {
        Chunked { r, left: 0, crlf: false, done: false }
    }
}

impl<R: BufRead> Chunked<R> {
    fn line(&mut self) -> std::io::Result<String> {
        let mut line = String::new();
        if self.r.read_line(&mut line)? == 0 || !line.ends_with('\n') {
            return Err(early_eof("connection closed before the last chunk".into()));
        }
        Ok(line)
    }
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            if self.crlf {
                if !self.line()?.trim().is_empty() {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk longer than its size"));
                }
                self.crlf = false;
            }
            let line = self.line()?;
            let size = line.trim().split(';').next().unwrap_or("").trim();
            self.left = usize::from_str_radix(size, 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad chunk size {size:?}")))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let max = buf.len().min(self.left);
        let n = self.r.read(&mut buf[..max])?;
        if n == 0 {
            return Err(early_eof(format!("connection closed with {} bytes of a chunk still to come", self.left)));
        }
        self.left -= n;
        self.crlf = self.left == 0;
        Ok(n)
    }
}

/// A bucket policy that allows everything on `bucket` and nothing else.
pub fn bucket_policy(bucket: &str) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": ["s3:*"], "Resource": [format!("arn:aws:s3:::{bucket}"), format!("arn:aws:s3:::{bucket}/*")]}],
    })
    .to_string()
}

/// S3 bucket names: 3-63 of lowercase letters, digits, `-` and `.`.
pub fn valid_bucket(b: &str) -> bool {
    (3..=63).contains(&b.len())
        && b.bytes().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-' || x == b'.')
        && b.as_bytes()[0].is_ascii_alphanumeric()
        && b.as_bytes()[b.len() - 1].is_ascii_alphanumeric()
}

/// SigV4 presigned URLs last at most a week.
pub const MAX_PRESIGN_SECS: u64 = 7 * 86400;

/// An object key someone may get a presigned URL for: not empty, no `.` or `..` segment
/// (a server that resolves those would sign a path into another bucket), no control
/// characters.
pub fn check_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("the object key is empty");
    }
    if key.split('/').any(|seg| seg == "." || seg == "..") {
        bail!("{key:?}: `.` and `..` aren't allowed in an object key");
    }
    if key.chars().any(char::is_control) {
        bail!("{key:?}: control characters aren't allowed in an object key");
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub modified: String,
}

impl S3 {
    fn region(&self) -> &str {
        self.region.as_deref().filter(|r| !r.is_empty()).unwrap_or(REGION)
    }

    /// Where requests go; an error for anything but `http://` and `https://` endpoints.
    fn target(&self) -> Result<Target> {
        target(self.endpoint.trim_end_matches('/')).map(|(t, _)| t)
    }

    /// One signed request. `path` is unencoded (`/bucket/key`).
    pub fn request(&self, method: &str, path: &str, query: &[(String, String)], body: Body) -> Result<Response> {
        self.request_with(method, path, query, &[], body)
    }

    /// [`S3::request`] with more `headers`, signed too.
    fn request_with(&self, method: &str, path: &str, query: &[(String, String)], headers: &[(&str, &str)], body: Body) -> Result<Response> {
        let t = self.target()?;
        let path = uri_encode(path, true);
        let q = canonical_query(query);
        let (payload, len) = match &body {
            Body::Empty => (sha256_hex(b""), 0),
            Body::Bytes(b) => (sha256_hex(b), b.len() as u64),
            Body::File(p) => ("UNSIGNED-PAYLOAD".to_string(), std::fs::metadata(p).with_context(|| format!("{}", p.display()))?.len()),
        };
        // Object bodies, and anything streamed from a file, may be slow; the rest is metadata.
        let slow = matches!(body, Body::File(_)) || (method == "GET" && path.trim_start_matches('/').contains('/'));
        let date = amz_date(now());
        let mut signed = vec![
            ("host".to_string(), t.host.clone()),
            ("x-amz-content-sha256".to_string(), payload.clone()),
            ("x-amz-date".to_string(), date.clone()),
        ];
        signed.extend(headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.to_string())));
        signed.sort();
        let names: Vec<&str> = signed.iter().map(|(k, _)| k.as_str()).collect();
        let region = self.region();
        let sig = signature(&self.secret_key, region, &date, method, &path, &q, &signed, &payload);
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{region}/s3/aws4_request, SignedHeaders={}, Signature={sig}",
            self.access_key,
            &date[..8],
            names.join(";")
        );
        let target = if q.is_empty() { path } else { format!("{path}?{q}") };
        let mut extra = format!("x-amz-date: {date}\r\nx-amz-content-sha256: {payload}\r\nAuthorization: {auth}\r\n");
        for (k, v) in headers {
            extra.push_str(&format!("{k}: {v}\r\n"));
        }
        raw(method, &t, &target, &extra, len, body, slow)
    }
}

/// `http://` or `https://`, and `host:port` (the scheme's port when the URL gives none).
#[derive(Debug, Clone, PartialEq)]
struct Target {
    tls: bool,
    /// What goes in `Host` and the signature: the URL's own spelling.
    host: String,
    /// What to dial.
    dial: String,
}

fn target(url: &str) -> Result<(Target, &str)> {
    let (tls, rest) = match (url.strip_prefix("http://"), url.strip_prefix("https://")) {
        (Some(r), _) => (false, r),
        (_, Some(r)) => (true, r),
        _ => bail!("{url}: storage endpoints are http:// or https://"),
    };
    let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
    if host.is_empty() {
        bail!("{url}: no host");
    }
    let has_port = host.rsplit_once(':').is_some_and(|(h, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && !h.ends_with(':'))
        && (!host.starts_with('[') || host.contains("]:"));
    let dial = if has_port { host.to_string() } else { format!("{host}:{}", if tls { 443 } else { 80 }) };
    Ok((Target { tls, host: host.to_string(), dial }, path))
}

/// Root certificates for `https://` endpoints: the Mac's (the system keychain, or
/// `SSL_CERT_FILE` when set).
static ROOTS: std::sync::Mutex<Option<std::sync::Arc<rustls::ClientConfig>>> = std::sync::Mutex::new(None);
static EXTRA_ROOTS: std::sync::Mutex<Vec<rustls::pki_types::CertificateDer<'static>>> = std::sync::Mutex::new(Vec::new());

/// Also trusts `ca_pem` for `https://` endpoints. Outside tests a private CA goes in
/// `SSL_CERT_FILE`.
#[cfg(test)]
pub fn trust_ca(ca_pem: &str) -> Result<()> {
    use rustls::pki_types::pem::PemObject;
    let certs: Vec<_> =
        rustls::pki_types::CertificateDer::pem_slice_iter(ca_pem.as_bytes()).collect::<Result<_, _>>().map_err(|e| anyhow!("CA: {e}"))?;
    EXTRA_ROOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(certs);
    *ROOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    Ok(())
}

fn tls_config() -> Result<std::sync::Arc<rustls::ClientConfig>> {
    static NATIVE: std::sync::OnceLock<Vec<rustls::pki_types::CertificateDer<'static>>> = std::sync::OnceLock::new();
    let mut cached = ROOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = cached.as_ref() {
        return Ok(c.clone());
    }
    let native = NATIVE.get_or_init(|| rustls_native_certs::load_native_certs().certs);
    let mut roots = rustls::RootCertStore::empty();
    // One unreadable system certificate shouldn't stop the others from working.
    roots.add_parsable_certificates(native.iter().cloned());
    for c in EXTRA_ROOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter() {
        roots.add(c.clone())?;
    }
    if roots.is_empty() {
        bail!("no trusted root certificates for https storage endpoints");
    }
    let cfg = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let cfg = std::sync::Arc::new(cfg);
    *cached = Some(cfg.clone());
    Ok(cfg)
}

/// How long one read may wait. Object bodies get longer: RustFS can take a while to
/// answer a big upload. Everything else is metadata, and a store that stalls on that
/// shouldn't hold a deploy for five minutes.
const META_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 30 });
const BODY_TIMEOUT: Duration = Duration::from_secs(300);
/// A peer that stops reading an upload.
const WRITE_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 60 });
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Error replies are read this far, at most.
const ERROR_BODY_MAX: u64 = 64 * 1024;

/// Every address `dial` resolves to, in order, until one connects.
fn connect(dial: &str) -> Result<TcpStream> {
    let addrs: Vec<_> = dial.to_socket_addrs().with_context(|| format!("resolve {dial}"))?.collect();
    let mut last = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e).with_context(|| format!("connect {dial}")),
        None => bail!("can't resolve {dial}"),
    }
}

/// One HTTP/1.1 request to `t` with `extra` headers (CRLF-terminated lines). `slow` gives
/// reads [`BODY_TIMEOUT`] instead of [`META_TIMEOUT`].
fn raw(method: &str, t: &Target, path: &str, extra: &str, len: u64, body: Body, slow: bool) -> Result<Response> {
    let tcp = connect(&t.dial)?;
    tcp.set_read_timeout(Some(if slow { BODY_TIMEOUT } else { META_TIMEOUT }))?;
    tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut s = if t.tls {
        let name = rustls::pki_types::ServerName::try_from(
            t.host.rsplit_once(':').map_or(t.host.as_str(), |(h, _)| h).trim_matches(['[', ']']).to_string(),
        )
        .map_err(|e| anyhow!("{}: {e}", t.host))?;
        let conn = rustls::ClientConnection::new(tls_config()?, name)?;
        crate::tls::Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        crate::tls::Stream::Plain(tcp)
    };
    let head = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n{extra}Content-Length: {len}\r\nConnection: close\r\n\r\n", t.host);
    s.write_all(head.as_bytes())?;
    match body {
        Body::Empty => {}
        Body::Bytes(b) => s.write_all(b)?,
        Body::File(p) => {
            std::io::copy(&mut std::fs::File::open(p)?, &mut s)?;
        }
    }
    s.flush()?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let host = &t.host;
    let status: u16 = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("bad response from {host}"))?;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let body = framed(method, status, &headers, r).with_context(|| format!("{method} {path} on {host}"))?;
    Ok(Response { status, body })
}

/// The reply body by its framing. A successful reply with no length, a length that isn't one
/// number, or a transfer coding other than plain `chunked` is an error: read to the close,
/// a cut connection would look like a complete object.
fn framed<R: BufRead + Send + 'static>(method: &str, status: u16, headers: &[(String, String)], r: R) -> Result<Box<dyn Read + Send>> {
    let all = |k: &str| headers.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
    if method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status) {
        return Ok(Box::new(std::io::empty()));
    }
    let te = all("transfer-encoding");
    let cl = all("content-length");
    let ok = (200..300).contains(&status);
    if !te.is_empty() {
        if te.len() == 1 && te[0].eq_ignore_ascii_case("chunked") {
            return Ok(Box::new(Chunked::new(r)));
        }
        if ok {
            bail!("unsupported Transfer-Encoding {:?}", te.join(", "));
        }
    } else if let [n] = cl.as_slice() {
        if let Ok(n) = n.parse::<u64>() {
            return Ok(Box::new(Exact { r, left: if ok { n } else { n.min(ERROR_BODY_MAX) } }));
        }
        if ok {
            bail!("bad Content-Length {n:?}");
        }
    } else if ok {
        bail!(if cl.is_empty() {
            "no Content-Length or chunked framing".to_string()
        } else {
            format!("{} Content-Length headers", cl.len())
        });
    }
    // An error reply: only its message is read, so whatever comes is enough.
    Ok(Box::new(r.take(ERROR_BODY_MAX)))
}

/// GET a presigned URL into `w`; returns the size.
pub fn url_get(url: &str, w: &mut impl Write) -> Result<u64> {
    let (t, path) = target(url)?;
    let mut r = raw("GET", &t, &format!("/{path}"), "", 0, Body::Empty, true)?.ok("get")?;
    Ok(std::io::copy(&mut r.body, w)?)
}

/// PUT to a presigned URL.
pub fn url_put(url: &str, body: Body) -> Result<()> {
    let (t, path) = target(url)?;
    let len = match &body {
        Body::Empty => 0,
        Body::Bytes(b) => b.len() as u64,
        Body::File(p) => std::fs::metadata(p)?.len(),
    };
    raw("PUT", &t, &format!("/{path}"), "", len, body, true)?.ok("put").map(|_| ())
}

impl S3 {
    /// Creates `bucket`; one that exists already is fine.
    pub fn create_bucket(&self, bucket: &str) -> Result<()> {
        let r = self.request("PUT", &format!("/{bucket}"), &[], Body::Empty)?;
        if r.status == 409 {
            return Ok(());
        }
        r.ok(&format!("create bucket {bucket}")).map(|_| ())
    }

    /// True on 200, false on 404. Any other answer (403, 500, ...) is an error, not "absent".
    pub fn bucket_exists(&self, bucket: &str) -> Result<bool> {
        match self.request("HEAD", &format!("/{bucket}"), &[], Body::Empty)?.status {
            200 => Ok(true),
            404 => Ok(false),
            status => bail!("check bucket {bucket}: HTTP {status}"),
        }
    }

    /// Every object under `prefix` (all pages).
    pub fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<Object>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (page, next) = self.list_page(bucket, prefix, None, token.as_deref())?;
            out.extend(page);
            match next {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
    }

    /// One page of ListObjectsV2 (at most `max` keys, the server's page size when None) and
    /// the token for the next page, if there is one.
    pub fn list_page(&self, bucket: &str, prefix: &str, max: Option<usize>, token: Option<&str>) -> Result<(Vec<Object>, Option<String>)> {
        let mut q = vec![("list-type".to_string(), "2".to_string()), ("prefix".to_string(), prefix.to_string())];
        if let Some(m) = max {
            q.push(("max-keys".into(), m.to_string()));
        }
        if let Some(t) = token {
            q.push(("continuation-token".into(), t.to_string()));
        }
        let xml = self.request("GET", &format!("/{bucket}"), &q, Body::Empty)?.ok(&format!("list {bucket}"))?.text()?;
        let out = xml_raw(&xml, "Contents")
            .into_iter()
            .map(|c| Object {
                key: xml_values(c, "Key").into_iter().next().unwrap_or_default(),
                size: xml_values(c, "Size").into_iter().next().and_then(|s| s.parse().ok()).unwrap_or(0),
                modified: xml_values(c, "LastModified").into_iter().next().unwrap_or_default(),
            })
            .collect();
        let truncated = xml_values(&xml, "IsTruncated").first().map(String::as_str) == Some("true");
        let next = xml_values(&xml, "NextContinuationToken").into_iter().next().filter(|_| truncated);
        Ok((out, next))
    }

    /// Whether `bucket` holds no objects under `prefix` (one key is enough to say no).
    pub fn is_empty(&self, bucket: &str, prefix: &str) -> Result<bool> {
        Ok(self.list_page(bucket, prefix, Some(1), None)?.0.is_empty())
    }

    /// Deletes `bucket`; with `force` everything in it first: every version and delete marker
    /// (an app may have turned versioning on with its own key) and unfinished multipart
    /// uploads. Without `force` a non-empty bucket is an error. Ok only when the DELETE
    /// succeeded or the bucket is confirmed absent (404).
    pub fn delete_bucket(&self, bucket: &str, force: bool) -> Result<()> {
        if !self.bucket_exists(bucket)? {
            return Ok(());
        }
        if force {
            self.delete_versions(bucket)?;
            self.abort_uploads(bucket)?;
        }
        let r = self.request("DELETE", &format!("/{bucket}"), &[], Body::Empty)?;
        if r.status == 404 {
            return Ok(());
        }
        r.ok(&format!("delete bucket {bucket}")).map(|_| ())
    }

    /// Every object version and delete marker in `bucket`, a page (1000) at a time, removed
    /// with one multi-object delete per page. In a bucket that never had versioning each
    /// object is one version, `null`.
    fn delete_versions(&self, bucket: &str) -> Result<()> {
        loop {
            let q = [("versions".to_string(), String::new()), ("max-keys".to_string(), "1000".to_string())];
            let xml = self.request("GET", &format!("/{bucket}"), &q, Body::Empty)?.ok(&format!("list versions of {bucket}"))?.text()?;
            let mut page: Vec<(String, Option<String>)> = Vec::new();
            for tag in ["Version", "DeleteMarker"] {
                for v in xml_raw(&xml, tag) {
                    let key = xml_values(v, "Key").into_iter().next().unwrap_or_default();
                    page.push((key, xml_values(v, "VersionId").into_iter().next()));
                }
            }
            if page.is_empty() {
                return Ok(());
            }
            self.delete_many(bucket, &page)?;
            // Deleted ones don't come back, so the next request starts from what's left.
        }
    }

    /// `POST /bucket?delete` for up to 1000 `(key, version)` pairs.
    pub fn delete_many(&self, bucket: &str, objects: &[(String, Option<String>)]) -> Result<()> {
        for chunk in objects.chunks(1000) {
            let mut xml = String::from("<Delete><Quiet>true</Quiet>");
            for (k, v) in chunk {
                xml.push_str(&format!("<Object><Key>{}</Key>", xml_escape(k)));
                if let Some(v) = v {
                    xml.push_str(&format!("<VersionId>{}</VersionId>", xml_escape(v)));
                }
                xml.push_str("</Object>");
            }
            xml.push_str("</Delete>");
            // S3 wants an integrity header on this call: Content-MD5 or a checksum.
            let sum = base64(&Sha256::digest(xml.as_bytes()));
            let q = [("delete".to_string(), String::new())];
            let headers = [("x-amz-checksum-sha256", sum.as_str()), ("Content-Type", "application/xml")];
            let text = self
                .request_with("POST", &format!("/{bucket}"), &q, &headers, Body::Bytes(xml.as_bytes()))?
                .ok(&format!("delete objects in {bucket}"))?
                .text()?;
            // Quiet mode lists only what failed.
            if let Some(e) = xml_raw(&text, "Error").first() {
                let key = xml_values(e, "Key").into_iter().next().unwrap_or_default();
                let msg =
                    xml_values(e, "Message").into_iter().next().or_else(|| xml_values(e, "Code").into_iter().next()).unwrap_or_default();
                bail!("delete {bucket}/{key}: {msg}");
            }
        }
        Ok(())
    }

    /// Aborts unfinished multipart uploads in `bucket`.
    fn abort_uploads(&self, bucket: &str) -> Result<()> {
        let q = [("uploads".to_string(), String::new())];
        let xml = self.request("GET", &format!("/{bucket}"), &q, Body::Empty)?.ok(&format!("list uploads in {bucket}"))?.text()?;
        for u in xml_raw(&xml, "Upload") {
            let (Some(key), Some(id)) = (xml_values(u, "Key").into_iter().next(), xml_values(u, "UploadId").into_iter().next()) else {
                continue;
            };
            let r = self.request("DELETE", &format!("/{bucket}/{key}"), &[("uploadId".into(), id)], Body::Empty)?;
            if r.status != 404 {
                r.ok(&format!("abort upload {bucket}/{key}"))?;
            }
        }
        Ok(())
    }

    pub fn put(&self, bucket: &str, key: &str, body: Body) -> Result<()> {
        self.request("PUT", &format!("/{bucket}/{key}"), &[], body)?.ok(&format!("put {bucket}/{key}")).map(|_| ())
    }

    /// True on 200, false on 404; any other answer is an error, not "absent".
    pub fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        match self.request("HEAD", &format!("/{bucket}/{key}"), &[], Body::Empty)?.status {
            200 => Ok(true),
            404 => Ok(false),
            status => bail!("check {bucket}/{key}: HTTP {status}"),
        }
    }

    /// Streams the object into `w`; returns its size.
    pub fn get(&self, bucket: &str, key: &str, w: &mut impl Write) -> Result<u64> {
        let mut r = self.request("GET", &format!("/{bucket}/{key}"), &[], Body::Empty)?.ok(&format!("get {bucket}/{key}"))?;
        Ok(std::io::copy(&mut r.body, w)?)
    }

    /// A small object's bytes, or None when there is no such key (404).
    pub fn get_opt(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let r = self.request("GET", &format!("/{bucket}/{key}"), &[], Body::Empty)?;
        if r.status == 404 {
            return Ok(None);
        }
        let mut b = Vec::new();
        r.ok(&format!("get {bucket}/{key}"))?.body.read_to_end(&mut b)?;
        Ok(Some(b))
    }

    pub fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        self.request("DELETE", &format!("/{bucket}/{key}"), &[], Body::Empty)?.ok(&format!("delete {bucket}/{key}")).map(|_| ())
    }

    /// A GET URL anyone can use for `secs` seconds.
    pub fn presign(&self, bucket: &str, key: &str, secs: u64) -> Result<String> {
        self.presign_at("GET", bucket, key, secs, now())
    }

    /// A URL that allows one `method` (GET or PUT) on one object for `secs` seconds: what
    /// a node gets instead of the root keys.
    pub fn presign_method(&self, method: &str, bucket: &str, key: &str, secs: u64) -> Result<String> {
        self.presign_at(method, bucket, key, secs, now())
    }

    fn presign_at(&self, method: &str, bucket: &str, key: &str, secs: u64, t: u64) -> Result<String> {
        check_key(key)?;
        if !(1..=MAX_PRESIGN_SECS).contains(&secs) {
            bail!("a presigned URL lasts 1 to {MAX_PRESIGN_SECS} seconds (7 days), not {secs}");
        }
        let t_ = self.target()?;
        let region = self.region();
        let path = uri_encode(&format!("/{bucket}/{key}"), true);
        let date = amz_date(t);
        let q = vec![
            ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential".into(), format!("{}/{}/{region}/s3/aws4_request", self.access_key, &date[..8])),
            ("X-Amz-Date".into(), date.clone()),
            ("X-Amz-Expires".into(), secs.to_string()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
        ];
        let cq = canonical_query(&q);
        let sig = signature(&self.secret_key, region, &date, method, &path, &cq, &[("host".into(), t_.host.clone())], "UNSIGNED-PAYLOAD");
        let scheme = if t_.tls { "https" } else { "http" };
        Ok(format!("{scheme}://{}{path}?{cq}&X-Amz-Signature={sig}", t_.host))
    }

    // ---------------------------------------------------------------- admin (root keys)

    fn admin(&self, method: &str, op: &str, query: &[(&str, &str)], body: &[u8]) -> Result<String> {
        let q: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self.request(method, &format!("/rustfs/admin/v3/{op}"), &q, Body::Bytes(body))?.ok(op)?.text()
    }

    /// A user who may only touch `bucket` (canned policy `bucket-<bucket>`).
    pub fn add_bucket_user(&self, access_key: &str, secret_key: &str, bucket: &str) -> Result<()> {
        let user = serde_json::json!({"secretKey": secret_key, "status": "enabled"}).to_string();
        self.admin("PUT", "add-user", &[("accessKey", access_key)], user.as_bytes())?;
        let policy = format!("bucket-{bucket}");
        self.admin("PUT", "add-canned-policy", &[("name", &policy)], bucket_policy(bucket).as_bytes())?;
        self.admin("PUT", "set-user-or-group-policy", &[("policyName", &policy), ("userOrGroup", access_key), ("isGroup", "false")], b"")?;
        Ok(())
    }

    pub fn remove_user(&self, access_key: &str) -> Result<()> {
        self.admin("DELETE", "remove-user", &[("accessKey", access_key)], b"").map(|_| ())
    }

    pub fn remove_policy(&self, bucket: &str) -> Result<()> {
        self.admin("DELETE", "remove-canned-policy", &[("name", &format!("bucket-{bucket}"))], b"").map(|_| ())
    }
}

/// An S3 on 127.0.0.1 for tests: objects in memory, every bucket exists, ListObjectsV2 in
/// one page. `fail` answers the next matching request with an error status instead.
#[cfg(test)]
pub(crate) mod mock {
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub(crate) struct State {
        /// `bucket/key` -> bytes.
        pub objects: BTreeMap<String, Vec<u8>>,
        /// `METHOD /bucket/key`, in order.
        pub log: Vec<String>,
        /// (method, part of `/bucket/key`, status).
        fail: Vec<(String, String, u16)>,
        /// `bucket/key` with an older version kept (versioning on): only a delete by
        /// version removes it, and the bucket can't be deleted while any remain.
        pub old_versions: Vec<String>,
    }

    pub(crate) struct Mock {
        pub s3: super::S3,
        pub state: Arc<Mutex<State>>,
    }

    impl Mock {
        pub fn fail(&self, method: &str, path: &str, status: u16) {
            self.state.lock().unwrap().fail.push((method.into(), path.into(), status));
        }
        pub fn keys(&self) -> Vec<String> {
            self.state.lock().unwrap().objects.keys().cloned().collect()
        }
        /// Requests seen, `METHOD /path`.
        pub fn requests(&self) -> usize {
            self.state.lock().unwrap().log.len()
        }
    }

    pub(crate) fn start() -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s3 = super::S3 {
            endpoint: format!("http://{}", l.local_addr().unwrap()),
            access_key: "test".into(),
            secret_key: "test".into(),
            region: None,
        };
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let _ = serve(&st, s);
            }
        });
        Mock { s3, state }
    }

    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match (b[i], s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
                (b'%', Some(x)) => {
                    out.push(x);
                    i += 3;
                }
                (c, _) => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn serve(state: &Mutex<State>, mut s: TcpStream) -> std::io::Result<()> {
        let mut r = BufReader::new(s.try_clone()?);
        let mut line = String::new();
        r.read_line(&mut line)?;
        let mut words = line.split_whitespace();
        let (method, target) = (words.next().unwrap_or_default().to_string(), words.next().unwrap_or_default().to_string());
        let mut len = 0;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body)?;
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let path = decode(path);
        let mut st = state.lock().unwrap();
        st.log.push(format!("{method} {path}"));
        let (status, out) = match st.fail.iter().position(|(m, p, _)| *m == method && path.contains(p.as_str())) {
            Some(i) => (st.fail.remove(i).2, Vec::new()),
            None => answer(&mut st, &method, &path, query, body),
        };
        drop(st);
        write!(s, "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len())?;
        if method != "HEAD" {
            s.write_all(&out)?;
        }
        Ok(())
    }

    fn answer(st: &mut State, method: &str, path: &str, query: &str, body: Vec<u8>) -> (u16, Vec<u8>) {
        let path = path.trim_start_matches('/');
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        if key.is_empty() {
            let has = |q: &str| query.split('&').any(|kv| kv == q || kv.starts_with(&format!("{q}=")));
            let mine = |k: &String| k.starts_with(&format!("{bucket}/"));
            return match method {
                // Every object is one version, plus its kept older one; like RustFS, one page.
                "GET" if has("versions") => {
                    let mut xml = String::from("<ListVersionsResult>");
                    let current = st.objects.keys().filter(|k| mine(k)).map(|k| (k.clone(), "null"));
                    let old = st.old_versions.iter().filter(|k| mine(k)).map(|k| (k.clone(), "v1"));
                    for (k, v) in current.chain(old).collect::<Vec<_>>() {
                        let k = k[bucket.len() + 1..].replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                        xml.push_str(&format!("<Version><Key>{k}</Key><VersionId>{v}</VersionId></Version>"));
                    }
                    (200, (xml + "</ListVersionsResult>").into_bytes())
                }
                "GET" if has("uploads") => (200, b"<ListMultipartUploadsResult></ListMultipartUploadsResult>".to_vec()),
                "POST" if has("delete") => {
                    let xml = String::from_utf8_lossy(&body).into_owned();
                    for o in super::xml_raw(&xml, "Object") {
                        let key = format!("{bucket}/{}", super::xml_values(o, "Key").concat());
                        match super::xml_values(o, "VersionId").first().map(String::as_str) {
                            Some("v1") => st.old_versions.retain(|k| *k != key),
                            _ => drop(st.objects.remove(&key)),
                        }
                    }
                    (200, b"<DeleteResult></DeleteResult>".to_vec())
                }
                "GET" => {
                    let prefix = query.split('&').find_map(|kv| kv.strip_prefix("prefix=")).map(decode).unwrap_or_default();
                    let max =
                        query.split('&').find_map(|kv| kv.strip_prefix("max-keys=")).and_then(|m| m.parse().ok()).unwrap_or(usize::MAX);
                    let mut xml = String::from("<ListBucketResult><IsTruncated>false</IsTruncated>");
                    let under = format!("{bucket}/{prefix}");
                    for (k, v) in st.objects.iter().filter(|(k, _)| k.starts_with(&under)).take(max) {
                        let k = k[bucket.len() + 1..].replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                        xml.push_str(&format!("<Contents><Key>{k}</Key><Size>{}</Size></Contents>", v.len()));
                    }
                    (200, (xml + "</ListBucketResult>").into_bytes())
                }
                "DELETE" if st.objects.keys().chain(&st.old_versions).any(mine) => (
                    409,
                    b"<Error><Code>BucketNotEmpty</Code><Message>The bucket you tried to delete is not empty</Message></Error>".to_vec(),
                ),
                "DELETE" => (204, Vec::new()),
                _ => (200, Vec::new()),
            };
        }
        let full = format!("{bucket}/{key}");
        match method {
            "PUT" => {
                st.objects.insert(full, body);
                (200, Vec::new())
            }
            "GET" | "HEAD" => match st.objects.get(&full) {
                Some(b) => (200, b.clone()),
                None => (404, b"<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>".to_vec()),
            },
            "DELETE" => {
                st.objects.remove(&full);
                (204, Vec::new())
            }
            _ => (405, Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    // From the AWS SigV4 S3 examples ("GET Object" and "Query String Authentication").
    const AK: &str = "AKIAIOSFODNN7EXAMPLE";
    const SK: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    #[test]
    fn aws_get_object_example() {
        let empty = sha256_hex(b"");
        let headers = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("range".into(), "bytes=0-9".into()),
            ("x-amz-content-sha256".into(), empty.clone()),
            ("x-amz-date".into(), "20130524T000000Z".into()),
        ];
        let sig = signature(SK, REGION, "20130524T000000Z", "GET", "/test.txt", "", &headers, &empty);
        assert_eq!(sig, "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    }

    #[test]
    fn aws_presign_example() {
        // 2013-05-24T00:00:00Z. The example is virtual-hosted; same canonical request as
        // path-style with host examplebucket.s3.amazonaws.com and path /test.txt.
        let date = amz_date(1_369_353_600);
        assert_eq!(date, "20130524T000000Z");
        let q = vec![
            ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential".into(), format!("{AK}/20130524/us-east-1/s3/aws4_request")),
            ("X-Amz-Date".into(), date.clone()),
            ("X-Amz-Expires".into(), "86400".into()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
        ];
        let sig = signature(
            SK,
            REGION,
            &date,
            "GET",
            "/test.txt",
            &canonical_query(&q),
            &[("host".into(), "examplebucket.s3.amazonaws.com".into())],
            "UNSIGNED-PAYLOAD",
        );
        assert_eq!(sig, "aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404");
    }

    /// Answers connection n with `replies[n]`; returns a client and the request lines seen.
    fn canned(replies: Vec<Vec<u8>>) -> (S3, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s3 =
            S3 { endpoint: format!("http://{}", l.local_addr().unwrap()), access_key: "a".into(), secret_key: "s".into(), region: None };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let got = seen.clone();
        std::thread::spawn(move || {
            for reply in replies {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                got.lock().unwrap().push(line.split_whitespace().take(2).collect::<Vec<_>>().join(" "));
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                        break;
                    }
                }
                s.write_all(&reply).unwrap();
            }
        });
        (s3, seen)
    }

    fn reply(status: &str, body: &str) -> Vec<u8> {
        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
    }

    fn early_eof(e: &anyhow::Error) -> bool {
        e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
    }

    #[test]
    fn xml_text_is_decoded_once() {
        assert_eq!(xml_values("<Key>report&amp;amp;old</Key>", "Key"), ["report&amp;old"]);
        assert_eq!(xml_values("<Key>&amp;lt;&lt;&#38;&#x26;&#60;&bogus;&#xZZ; & x</Key>", "Key"), ["&lt;<&&<&bogus;&#xZZ; & x"]);
        let xml = "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>a&amp;amp;b</Key><Size>3</Size></Contents>\
                   <Contents><Key>c</Key><Size>10</Size></Contents></ListBucketResult>";
        let c = xml_raw(xml, "Contents");
        assert_eq!(c.len(), 2);
        assert_eq!(xml_values(c[0], "Key"), ["a&amp;b"]);
    }

    #[test]
    fn force_delete_removes_keys_with_entities() {
        let m = mock::start();
        // Keys that literally contain entity text list as themselves, so these are what's deleted.
        m.s3.put("bucket", "report&amp;old", Body::Bytes(b"x")).unwrap();
        m.s3.put("bucket", "a<b>&#38;", Body::Bytes(b"y")).unwrap();
        let keys: Vec<String> = m.s3.list("bucket", "").unwrap().into_iter().map(|o| o.key).collect();
        assert_eq!(keys, ["a<b>&#38;", "report&amp;old"]);
        m.s3.delete_bucket("bucket", true).unwrap();
        assert!(m.keys().is_empty(), "{:?}", m.keys());
    }

    #[test]
    fn chunked_body() {
        let read = |raw: &[u8]| {
            let mut s = Vec::new();
            Chunked::new(raw).read_to_end(&mut s).map(|_| s)
        };
        assert_eq!(read(b"3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        for cut in [&b"3\r\nabc\r\n"[..], b"3\r\nab", b"3\r\nabc", b"3\r\nabc\r\n2"] {
            assert_eq!(read(cut).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof, "{:?}", String::from_utf8_lossy(cut));
        }
        assert_eq!(read(b"3\r\nabcd\r\n0\r\n\r\n").unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn short_bodies_are_errors() {
        let mut b = Vec::new();
        assert_eq!(Exact { r: &b"abcdef"[..], left: 4 }.read_to_end(&mut b).unwrap(), 4);
        assert_eq!(Exact { r: &b"abc"[..], left: 9 }.read_to_end(&mut b).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);

        // Nine bytes promised and three sent, then a chunked body without its last chunk.
        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabc".to_vec();
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n".to_vec();
        // No framing, a length that isn't one number, a coding we can't read: a cut
        // connection would look complete, so they're refused.
        let unframed = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc".to_vec();
        let doubled = b"HTTP/1.1 200 OK\r\nContent-Length: 9, 9\r\nConnection: close\r\n\r\nabcdefghi".to_vec();
        let gzip = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n0\r\n\r\n".to_vec();
        let (s3, _) =
            canned(vec![short.clone(), short, chunked, unframed, doubled, gzip, reply("404 Not Found", "x"), reply("200 OK", "abcdefghi")]);
        assert!(early_eof(&s3.get("b", "k", &mut Vec::new()).unwrap_err()));
        assert!(early_eof(&url_get(&s3.presign("b", "k", 60).unwrap(), &mut Vec::new()).unwrap_err()));
        assert!(early_eof(&s3.get("b", "k", &mut Vec::new()).unwrap_err()));
        for what in ["no Content-Length", "bad Content-Length", "Transfer-Encoding"] {
            let e = format!("{:#}", s3.get("b", "k", &mut Vec::new()).unwrap_err());
            assert!(e.contains(what), "{e}");
        }
        // An error reply's own framing doesn't matter; its message is still the error.
        assert!(s3.get_opt("b", "k").unwrap().is_none());
        let mut b = Vec::new();
        assert_eq!(s3.get("b", "k", &mut b).unwrap(), 9);
        assert_eq!(b, b"abcdefghi");
    }

    #[test]
    fn only_404_means_no_bucket() {
        let (s3, _) = canned(vec![reply("200 OK", ""), reply("404 Not Found", ""), reply("500 Internal Server Error", "")]);
        assert!(s3.bucket_exists("b").unwrap());
        assert!(!s3.bucket_exists("b").unwrap());
        assert!(s3.bucket_exists("b").is_err());

        // A failed HEAD is an error, and nothing is deleted.
        let (s3, seen) = canned(vec![reply("500 Internal Server Error", "")]);
        assert!(s3.delete_bucket("b", true).is_err());
        assert_eq!(*seen.lock().unwrap(), ["HEAD /b"]);
        // Already gone.
        let (s3, seen) = canned(vec![reply("404 Not Found", "")]);
        s3.delete_bucket("b", true).unwrap();
        assert_eq!(*seen.lock().unwrap(), ["HEAD /b"]);
        // The DELETE itself fails. Before it: HEAD, the version listing, the upload listing.
        let (versions, uploads) =
            ("<ListVersionsResult></ListVersionsResult>", "<ListMultipartUploadsResult></ListMultipartUploadsResult>");
        for status in ["503 Service Unavailable", "409 Conflict"] {
            let (s3, seen) = canned(vec![reply("200 OK", ""), reply("200 OK", versions), reply("200 OK", uploads), reply(status, "")]);
            assert!(s3.delete_bucket("b", true).is_err(), "{status}");
            assert_eq!(seen.lock().unwrap().last().unwrap(), "DELETE /b");
        }
        let (s3, _) = canned(vec![reply("200 OK", ""), reply("200 OK", versions), reply("200 OK", uploads), reply("204 No Content", "")]);
        s3.delete_bucket("b", true).unwrap();

        // An object check is the same: 403 and 500 are errors, not "no such object".
        let (s3, _) = canned(vec![reply("200 OK", ""), reply("404 Not Found", ""), reply("403 Forbidden", ""), reply("500 Oops", "")]);
        assert!(s3.exists("b", "k").unwrap());
        assert!(!s3.exists("b", "k").unwrap());
        assert!(s3.exists("b", "k").is_err());
        assert!(s3.exists("b", "k").is_err());
    }

    #[test]
    fn force_delete_removes_old_versions_in_few_requests() {
        let m = mock::start();
        for i in 0..2500 {
            m.state.lock().unwrap().objects.insert(format!("b/k{i}"), b"x".to_vec());
        }
        // An app turned versioning on and overwrote one: the old version stays after a plain
        // delete of every current object, and the bucket DELETE answers 409.
        m.state.lock().unwrap().old_versions.push("b/k1".into());
        let before = m.requests();
        m.s3.delete_bucket("b", true).unwrap();
        assert!(m.keys().is_empty() && m.state.lock().unwrap().old_versions.is_empty());
        // HEAD, versions, 3 batch deletes, versions again (empty), uploads, DELETE.
        assert!(m.requests() - before <= 8, "{} requests", m.requests() - before);
        assert!(m.s3.is_empty("b", "").unwrap());
    }

    #[test]
    fn a_stalled_store_fails_within_the_timeouts() {
        // Accepts, never reads or answers.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s3 =
            S3 { endpoint: format!("http://{}", l.local_addr().unwrap()), access_key: "a".into(), secret_key: "s".into(), region: None };
        std::thread::spawn(move || {
            let held: Vec<_> = l.incoming().take(2).collect();
            std::thread::sleep(Duration::from_secs(30));
            drop(held);
        });
        let t0 = std::time::Instant::now();
        assert!(s3.bucket_exists("b").is_err());
        let big = vec![0u8; 64 << 20];
        assert!(s3.put("b", "k", Body::Bytes(&big)).is_err());
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    }

    #[test]
    fn https_endpoints_check_the_certificate() {
        let dir = std::env::temp_dir().join(format!("rekcod-s3-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca = crate::tls::ensure_ca(&dir).unwrap();
        let (cert, key) = ca.issue(&["127.0.0.1".into()]).unwrap();
        let cfg = crate::tls::server_config(&cert, &key).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for tcp in l.incoming().flatten() {
                let Ok(mut s) = crate::tls::accept(Some(&cfg), tcp) else { continue };
                let mut r = BufReader::new(&mut s);
                let mut seen = String::new();
                while r.read_line(&mut seen).is_ok_and(|n| n > 2) {}
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
                let _ = s.flush();
            }
        });
        let s3 = S3 {
            endpoint: format!("https://127.0.0.1:{port}"),
            access_key: "a".into(),
            secret_key: "s".into(),
            region: Some("eu-west-1".into()),
        };
        // Not trusted yet: the system roots don't know this CA.
        assert!(s3.get("b", "k", &mut Vec::new()).is_err());
        trust_ca(&ca.pem).unwrap();
        let mut b = Vec::new();
        assert_eq!(s3.get("b", "k", &mut b).unwrap(), 5);
        assert_eq!(b, b"hello");
        let url = s3.presign("b", "k", 60).unwrap();
        assert!(url.starts_with(&format!("https://127.0.0.1:{port}/b/k?")) && url.contains("%2Feu-west-1%2Fs3%2F"), "{url}");
        assert_eq!(url_get(&url, &mut Vec::new()).unwrap(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presigned_urls_refuse_bad_keys_and_lifetimes() {
        let s3 = S3 { endpoint: "http://127.0.0.1:9".into(), access_key: "a".into(), secret_key: "s".into(), region: None };
        for key in ["", ".", "..", "a/../b", "../rekcod-backups/fleet.db", "a/./b", "a\nb"] {
            assert!(s3.presign("app-web", key, 60).is_err(), "{key:?}");
        }
        assert!(s3.presign("app-web", "a/b..c/.d", 60).is_ok());
        assert!(s3.presign("app-web", "k", 0).is_err());
        assert!(s3.presign("app-web", "k", MAX_PRESIGN_SECS + 1).is_err());
        assert!(s3.presign("app-web", "k", MAX_PRESIGN_SECS).is_ok());
        assert!(S3 { endpoint: "ftp://x".into(), ..s3 }.presign("b", "k", 60).is_err());
        assert_eq!(base64(b"abcd"), "YWJjZA==");
        assert_eq!(base64(b"abcde"), "YWJjZGU=");
    }

    /// Against a real RustFS (`rekcod storage serve` on a scratch directory):
    /// `REKCOD_TEST_RUSTFS=http://127.0.0.1:PORT REKCOD_TEST_RUSTFS_KEYS=ACCESS:SECRET
    /// cargo test -- --ignored rustfs_`. Checks the answers the destroy path relies on.
    #[test]
    #[ignore = "needs a RustFS; see the comment"]
    fn rustfs_deletes_versioned_buckets_and_refuses_bad_keys() {
        let endpoint = std::env::var("REKCOD_TEST_RUSTFS").expect("REKCOD_TEST_RUSTFS");
        let keys = std::env::var("REKCOD_TEST_RUSTFS_KEYS").expect("REKCOD_TEST_RUSTFS_KEYS");
        let (ak, sk) = keys.split_once(':').unwrap();
        let root = S3 { endpoint: endpoint.clone(), access_key: ak.into(), secret_key: sk.into(), region: None };
        let id = &crate::sandbox::new_id().unwrap()[..8];
        let bucket = format!("probe-{id}");
        let user = format!("probe-{id}");
        root.create_bucket(&bucket).unwrap();
        root.add_bucket_user(&user, "probe-secret-123456", &bucket).unwrap();
        let app = S3 { endpoint, access_key: user.clone(), secret_key: "probe-secret-123456".into(), region: None };
        // What a tenant's S3 library may do with its own key: versioning, overwrites, a
        // delete marker, a multipart upload it never finishes.
        let on = b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>";
        app.request("PUT", &format!("/{bucket}"), &[("versioning".into(), String::new())], Body::Bytes(on))
            .unwrap()
            .ok("versioning")
            .unwrap();
        app.put(&bucket, "a b+c&<d>.txt", Body::Bytes(b"1")).unwrap();
        app.put(&bucket, "a b+c&<d>.txt", Body::Bytes(b"2")).unwrap();
        app.delete(&bucket, "a b+c&<d>.txt").unwrap();
        app.request("POST", &format!("/{bucket}/upload"), &[("uploads".into(), String::new())], Body::Empty).unwrap().ok("upload").unwrap();
        for i in 0..1100 {
            app.put(&bucket, &format!("many/{i}"), Body::Bytes(b"x")).unwrap();
        }
        assert!(!root.is_empty(&bucket, "").unwrap());
        assert!(root.exists(&bucket, "many/1").unwrap() && !root.exists(&bucket, "nope").unwrap());
        let wrong = S3 { secret_key: "wrong".into(), ..root.clone() };
        assert!(wrong.exists(&bucket, "many/1").is_err(), "a 403 is not \"absent\"");
        // The app's presigned URL works, and stops working with its key.
        let url = app.presign(&bucket, "many/1", 60).unwrap();
        assert_eq!(url_get(&url, &mut Vec::new()).unwrap(), 1);
        assert!(app.presign(&bucket, "../rekcod-backups/x", 60).is_err());
        root.remove_user(&user).unwrap();
        assert!(url_get(&url, &mut Vec::new()).is_err());
        // Missing user: 404; missing policy: what `gone` treats as done.
        assert!(root.remove_user(&user).unwrap_err().to_string().ends_with("(404)"));
        root.remove_policy(&bucket).unwrap();
        let t0 = std::time::Instant::now();
        root.delete_bucket(&bucket, true).unwrap();
        eprintln!("delete_bucket of a versioned bucket with 1100 objects: {:.1}s", t0.elapsed().as_secs_f64());
        assert!(!root.bucket_exists(&bucket).unwrap());
    }

    #[test]
    fn bucket_names() {
        assert!(valid_bucket("app-web--team"));
        assert!(!valid_bucket("ab") && !valid_bucket("App") && !valid_bucket("-x-y") && !valid_bucket(&"a".repeat(64)));
    }
}
