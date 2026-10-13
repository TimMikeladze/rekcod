//! A small S3 client for RustFS (docs/rustfs.md): SigV4, path-style URLs, plain HTTP/1.1.
//! Covers what rekcod needs: buckets, objects streamed from and to files, presigned GETs,
//! and RustFS's MinIO-style admin API for per-app users and policies.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use hmac::Mac;
use sha2::{Digest, Sha256};

pub const REGION: &str = "us-east-1";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct S3 {
    /// `http://host:port`.
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
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

fn hex(b: &[u8]) -> String {
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
fn signature(secret: &str, date: &str, method: &str, path: &str, query: &str, headers: &[(String, String)], payload: &str) -> String {
    let canon_headers: String = headers.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let creq = format!("{method}\n{path}\n{query}\n{canon_headers}\n{}\n{payload}", signed.join(";"));
    let day = &date[..8];
    let scope = format!("{day}/{REGION}/s3/aws4_request");
    let sts = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", sha256_hex(creq.as_bytes()));
    let mut k = hmac(format!("AWS4{secret}").as_bytes(), day);
    for part in [REGION, "s3", "aws4_request"] {
        k = hmac(&k, part);
    }
    hex(&hmac(&k, &sts))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `<Tag>value</Tag>` contents, in order. Enough for ListObjectsV2 and error bodies.
pub fn xml_values(xml: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(after[..j].replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'"));
        rest = &after[j + close.len()..];
    }
    out
}

/// Reads a `Transfer-Encoding: chunked` body.
struct Chunked<R> {
    r: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            self.r.read_line(&mut line)?;
            if line.trim().is_empty() {
                // The CRLF after the previous chunk.
                line.clear();
                self.r.read_line(&mut line)?;
            }
            let size = line.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(size, 16).map_err(|_| std::io::Error::other(format!("bad chunk size {size:?}")))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let max = buf.len().min(self.left);
        let n = self.r.read(&mut buf[..max])?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n;
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub modified: String,
}

impl S3 {
    fn authority(&self) -> Result<String> {
        let rest = self.endpoint.strip_prefix("http://").ok_or_else(|| anyhow!("{}: only http:// endpoints", self.endpoint))?;
        Ok(rest.trim_end_matches('/').to_string())
    }

    /// One signed request. `path` is unencoded (`/bucket/key`).
    pub fn request(&self, method: &str, path: &str, query: &[(String, String)], body: Body) -> Result<Response> {
        let host = self.authority()?;
        let path = uri_encode(path, true);
        let q = canonical_query(query);
        let (payload, len) = match &body {
            Body::Empty => (sha256_hex(b""), 0),
            Body::Bytes(b) => (sha256_hex(b), b.len() as u64),
            Body::File(p) => ("UNSIGNED-PAYLOAD".to_string(), std::fs::metadata(p).with_context(|| format!("{}", p.display()))?.len()),
        };
        let date = amz_date(now());
        let headers = vec![
            ("host".to_string(), host.clone()),
            ("x-amz-content-sha256".to_string(), payload.clone()),
            ("x-amz-date".to_string(), date.clone()),
        ];
        let sig = signature(&self.secret_key, &date, method, &path, &q, &headers, &payload);
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{REGION}/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}",
            self.access_key,
            &date[..8]
        );
        let addr = host.to_socket_addrs()?.next().ok_or_else(|| anyhow!("can't resolve {host}"))?;
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).with_context(|| format!("connect {}", self.endpoint))?;
        s.set_read_timeout(Some(Duration::from_secs(300)))?;
        let target = if q.is_empty() { path } else { format!("{path}?{q}") };
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: {host}\r\nx-amz-date: {date}\r\nx-amz-content-sha256: {payload}\r\nAuthorization: {auth}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
        )?;
        match body {
            Body::Empty => {}
            Body::Bytes(b) => s.write_all(b)?,
            Body::File(p) => {
                std::io::copy(&mut std::fs::File::open(p)?, &mut s)?;
            }
        }
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line)?;
        let status: u16 =
            line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("bad response from {}", self.endpoint))?;
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
        let header = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let body: Box<dyn Read + Send> = if method == "HEAD" {
            Box::new(std::io::empty())
        } else if header("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
            Box::new(Chunked { r, left: 0, done: false })
        } else if let Some(n) = header("content-length").and_then(|n| n.parse::<u64>().ok()) {
            Box::new(r.take(n))
        } else {
            Box::new(r)
        };
        Ok(Response { status, body })
    }

    /// Creates `bucket`; one that exists already is fine.
    pub fn create_bucket(&self, bucket: &str) -> Result<()> {
        let r = self.request("PUT", &format!("/{bucket}"), &[], Body::Empty)?;
        if r.status == 409 {
            return Ok(());
        }
        r.ok(&format!("create bucket {bucket}")).map(|_| ())
    }

    pub fn bucket_exists(&self, bucket: &str) -> Result<bool> {
        Ok(self.request("HEAD", &format!("/{bucket}"), &[], Body::Empty)?.status == 200)
    }

    /// Every object under `prefix` (all pages).
    pub fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<Object>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = vec![("list-type".to_string(), "2".to_string()), ("prefix".to_string(), prefix.to_string())];
            if let Some(t) = &token {
                q.push(("continuation-token".into(), t.clone()));
            }
            let xml = self.request("GET", &format!("/{bucket}"), &q, Body::Empty)?.ok(&format!("list {bucket}"))?.text()?;
            for c in xml_values(&xml, "Contents") {
                out.push(Object {
                    key: xml_values(&c, "Key").into_iter().next().unwrap_or_default(),
                    size: xml_values(&c, "Size").into_iter().next().and_then(|s| s.parse().ok()).unwrap_or(0),
                    modified: xml_values(&c, "LastModified").into_iter().next().unwrap_or_default(),
                });
            }
            token = xml_values(&xml, "NextContinuationToken").into_iter().next();
            if token.is_none() || xml_values(&xml, "IsTruncated").first().map(String::as_str) != Some("true") {
                return Ok(out);
            }
        }
    }

    /// Deletes `bucket`; with `force` its objects first, else a non-empty bucket is an error.
    pub fn delete_bucket(&self, bucket: &str, force: bool) -> Result<()> {
        if !self.bucket_exists(bucket)? {
            return Ok(());
        }
        if force {
            for o in self.list(bucket, "")? {
                self.delete(bucket, &o.key)?;
            }
        }
        let r = self.request("DELETE", &format!("/{bucket}"), &[], Body::Empty)?;
        if r.status == 404 {
            return Ok(());
        }
        r.ok(&format!("delete bucket {bucket}")).map(|_| ())
    }

    pub fn put(&self, bucket: &str, key: &str, body: Body) -> Result<()> {
        self.request("PUT", &format!("/{bucket}/{key}"), &[], body)?.ok(&format!("put {bucket}/{key}")).map(|_| ())
    }

    pub fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        Ok(self.request("HEAD", &format!("/{bucket}/{key}"), &[], Body::Empty)?.status == 200)
    }

    /// Streams the object into `w`; returns its size.
    pub fn get(&self, bucket: &str, key: &str, w: &mut impl Write) -> Result<u64> {
        let mut r = self.request("GET", &format!("/{bucket}/{key}"), &[], Body::Empty)?.ok(&format!("get {bucket}/{key}"))?;
        Ok(std::io::copy(&mut r.body, w)?)
    }

    pub fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        self.request("DELETE", &format!("/{bucket}/{key}"), &[], Body::Empty)?.ok(&format!("delete {bucket}/{key}")).map(|_| ())
    }

    /// A GET URL anyone can use for `secs` seconds.
    pub fn presign(&self, bucket: &str, key: &str, secs: u64) -> Result<String> {
        self.presign_at(bucket, key, secs, now())
    }

    fn presign_at(&self, bucket: &str, key: &str, secs: u64, t: u64) -> Result<String> {
        let host = self.authority()?;
        let path = uri_encode(&format!("/{bucket}/{key}"), true);
        let date = amz_date(t);
        let q = vec![
            ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential".into(), format!("{}/{}/{REGION}/s3/aws4_request", self.access_key, &date[..8])),
            ("X-Amz-Date".into(), date.clone()),
            ("X-Amz-Expires".into(), secs.to_string()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
        ];
        let cq = canonical_query(&q);
        let sig = signature(&self.secret_key, &date, "GET", &path, &cq, &[("host".into(), host.clone())], "UNSIGNED-PAYLOAD");
        Ok(format!("http://{host}{path}?{cq}&X-Amz-Signature={sig}"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let sig = signature(SK, "20130524T000000Z", "GET", "/test.txt", "", &headers, &empty);
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
            &date,
            "GET",
            "/test.txt",
            &canonical_query(&q),
            &[("host".into(), "examplebucket.s3.amazonaws.com".into())],
            "UNSIGNED-PAYLOAD",
        );
        assert_eq!(sig, "aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404");
    }

    #[test]
    fn list_xml() {
        let xml = "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>a&amp;b</Key><Size>3</Size></Contents>\
                   <Contents><Key>c</Key><Size>10</Size></Contents></ListBucketResult>";
        let c = xml_values(xml, "Contents");
        assert_eq!(c.len(), 2);
        assert_eq!(xml_values(&c[0], "Key"), ["a&b"]);
    }

    #[test]
    fn chunked_body() {
        let raw = b"3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        let mut s = String::new();
        Chunked { r: &raw[..], left: 0, done: false }.read_to_string(&mut s).unwrap();
        assert_eq!(s, "abcde");
    }

    #[test]
    fn bucket_names() {
        assert!(valid_bucket("app-web--team"));
        assert!(!valid_bucket("ab") && !valid_bucket("App") && !valid_bucket("-x-y") && !valid_bucket(&"a".repeat(64)));
    }
}
