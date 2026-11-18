//! TLS for the fleet's control plane (docs/fleet-hardening.md, "Transport").
//!
//! The controller makes its own CA on first start (`controller/ca.pem`, `ca.key`) and a
//! fresh server certificate from it at every start. Nodes get theirs from the controller
//! when they join. Clients (the CLI, edges, nodes) trust only that CA, which they fetch once
//! and check against a fingerprint the controller printed: `sha256:<hex of the CA's DER>`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The fleet's CA: its certificate (PEM, what clients trust) and what's needed to sign with it.
pub struct Ca {
    pub pem: String,
    params: rcgen::CertificateParams,
    key: rcgen::KeyPair,
}

fn ca_params(common_name: &str) -> Result<rcgen::CertificateParams> {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new())?;
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.distinguished_name.push(rcgen::DnType::CommonName, common_name);
    p.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
    p.not_before = rcgen::date_time_ymd(2025, 1, 1);
    p.not_after = rcgen::date_time_ymd(2045, 1, 1);
    Ok(p)
}

/// Loads `dir/ca.pem` and `dir/ca.key`, making them on first use. This CA signs only the
/// controller's own certificate; it's what CLIs and edges trust.
pub fn ensure_ca(dir: &Path) -> Result<Ca> {
    ensure_named(dir, "ca", "rekcod fleet CA")
}

/// `dir/node-ca.pem`: signs node certificates, and only the controller trusts it. A node
/// (anyone with the join token, before approval) gets a certificate from this one, so it can
/// never pass for the controller to a CLI or an edge.
pub fn ensure_node_ca(dir: &Path) -> Result<Ca> {
    ensure_named(dir, "node-ca", "rekcod node CA")
}

fn ensure_named(dir: &Path, name: &str, common_name: &str) -> Result<Ca> {
    let (pem_path, key_path) = (dir.join(format!("{name}.pem")), dir.join(format!("{name}.key")));
    if let (Ok(pem), Ok(key)) = (std::fs::read_to_string(&pem_path), std::fs::read_to_string(&key_path)) {
        let key = rcgen::KeyPair::from_pem(&key).with_context(|| format!("controller/{name}.key"))?;
        return Ok(Ca { pem, params: ca_params(common_name)?, key });
    }
    let key = rcgen::KeyPair::generate()?;
    let params = ca_params(common_name)?;
    let pem = ca_params(common_name)?.self_signed(&key)?.pem();
    crate::paths::write_private(&key_path, key.serialize_pem().as_bytes())?;
    std::fs::write(&pem_path, &pem)?;
    Ok(Ca { pem, params, key })
}

impl Ca {
    /// A certificate for `names` (host names or IP addresses), valid for two years, and its
    /// key, both PEM.
    pub fn issue(&self, names: &[String]) -> Result<(String, String)> {
        let mut p = rcgen::CertificateParams::new(Vec::<String>::new())?;
        for n in names {
            match n.parse::<IpAddr>() {
                Ok(ip) => p.subject_alt_names.push(rcgen::SanType::IpAddress(ip)),
                Err(_) => p.subject_alt_names.push(rcgen::SanType::DnsName(n.clone().try_into()?)),
            }
        }
        p.distinguished_name.push(rcgen::DnType::CommonName, names.first().cloned().unwrap_or_default());
        p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        p.use_authority_key_identifier_extension = true;
        let now = std::time::SystemTime::now();
        p.not_before = now.checked_sub(std::time::Duration::from_secs(86400)).unwrap_or(now).into();
        p.not_after = (now + std::time::Duration::from_secs(2 * 365 * 86400)).into();
        let key = rcgen::KeyPair::generate()?;
        // Same name and key as ca.pem, so what it signs verifies against ca.pem.
        let issuer = self.params.clone().self_signed(&self.key)?;
        let cert = p.signed_by(&key, &issuer, &self.key)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    pub fn fingerprint(&self) -> Result<String> {
        fingerprint(&self.pem)
    }
}

/// The one certificate in a CA pin. A pin is a single certificate: `fingerprint` hashes it and
/// `roots` trusts it, so a bundle with a second certificate behind the pinned one is refused
/// rather than trusted along with it.
pub fn ca_cert(pem: &str) -> Result<CertificateDer<'static>> {
    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<Result<_, _>>().map_err(|e| anyhow!("not a certificate: {e}"))?;
    match <[_; 1]>::try_from(certs) {
        Ok([c]) => Ok(c),
        Err(v) if v.is_empty() => bail!("no certificate"),
        Err(v) => bail!("{} certificates where the CA pin is one", v.len()),
    }
}

/// `sha256:<hex>` of the certificate in `pem`, which must hold exactly one.
pub fn fingerprint(pem: &str) -> Result<String> {
    Ok(der_fingerprint(&ca_cert(pem)?))
}

fn der_fingerprint(der: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{}", sha2::Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn pem_of(der: &[u8]) -> String {
    let b64 = crate::daemon::base64(der);
    let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
    format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
}

pub fn server_config(cert_pem: &str, key_pem: &str) -> Result<Arc<rustls::ServerConfig>> {
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(cert_pem.as_bytes()).collect::<Result<_, _>>().map_err(|e| anyhow!("certificate: {e}"))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| anyhow!("key: {e}"))?;
    let cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    Ok(Arc::new(cfg))
}

fn roots(ca_pem: &str) -> Result<Arc<rustls::RootCertStore>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_cert(ca_pem).context("CA")?)?;
    Ok(Arc::new(roots))
}

fn client_config(ca_pem: &str) -> Result<Arc<rustls::ClientConfig>> {
    let cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots(ca_pem)?)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// CAs to trust, by `host:port`, plus one for any other https URL (a controller trusts its
/// own CA for every node; a node trusts its controller's).
static TRUST: Mutex<Option<HashMap<String, Arc<rustls::ClientConfig>>>> = Mutex::new(None);
static DEFAULT_TRUST: OnceLock<(Arc<rustls::ClientConfig>, Arc<rustls::RootCertStore>)> = OnceLock::new();

pub fn trust(authority: &str, ca_pem: &str) -> Result<()> {
    let cfg = client_config(ca_pem)?;
    TRUST.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_default().insert(authority.to_string(), cfg);
    Ok(())
}

pub fn trust_by_default(ca_pem: &str) -> Result<()> {
    let _ = DEFAULT_TRUST.set((client_config(ca_pem)?, roots(ca_pem)?));
    Ok(())
}

/// Node certificates name the node, not its host: several nodes can share a host, and a
/// join-token holder gets a certificate only its own node ID accepts.
pub fn node_name(id: &str) -> String {
    format!("{id}.node.rekcod.invalid")
}

/// A node the controller dials at `host:port`: its ID, and whether its certificate is from
/// before node names (it names the host).
static NODES: Mutex<Option<HashMap<String, (String, bool)>>> = Mutex::new(None);

/// The controller expects node `id`'s certificate from `authority`.
pub fn expect_node(authority: &str, id: &str, host_named: bool) {
    NODES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_default()
        .insert(authority.to_ascii_lowercase(), (id.to_string(), host_named));
}

/// Drops what the controller expects at `authority`, if that's node `id`: another node's row
/// at the same address going away leaves this one's expectation alone.
pub fn forget_node(authority: &str, id: &str) {
    if let Some(m) = NODES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
        let key = authority.to_ascii_lowercase();
        if m.get(&key).is_some_and(|(n, _)| n == id) {
            m.remove(&key);
        }
    }
}

fn expected_node(authority: &str) -> Option<(String, bool)> {
    NODES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().and_then(|m| m.get(&authority.to_ascii_lowercase()).cloned())
}

/// Checks a node's certificate against its node name; one issued before node names may
/// name its host instead.
#[derive(Debug)]
struct NodeCert {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    host: Option<ServerName<'static>>,
}

impl rustls::client::danger::ServerCertVerifier for NodeCert {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        inter: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.inner.verify_server_cert(end, inter, name, ocsp, now).or_else(|e| match &self.host {
            Some(h) => self.inner.verify_server_cert(end, inter, h, ocsp, now).map_err(|_| e),
            None => Err(e),
        })
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A client config and server name for a node the controller expects at `authority`.
fn node_config(
    authority: &str,
    id: &str,
    host_named: bool,
    roots: Arc<rustls::RootCertStore>,
) -> Result<(Arc<rustls::ClientConfig>, ServerName<'static>)> {
    let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider()).build()?;
    let host = if host_named {
        Some(ServerName::try_from(host_of(authority).to_string()).map_err(|e| anyhow!("{authority}: {e}"))?)
    } else {
        None
    };
    let cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NodeCert { inner, host }))
        .with_no_client_auth();
    Ok((Arc::new(cfg), ServerName::try_from(node_name(id)).map_err(|e| anyhow!("node {id}: {e}"))?))
}

fn trusted(authority: &str) -> Option<Arc<rustls::ClientConfig>> {
    let t = TRUST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    t.as_ref().and_then(|m| m.get(authority).cloned()).or_else(|| DEFAULT_TRUST.get().map(|d| d.0.clone()))
}

/// A client connection, plain or TLS.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => match s.read(buf) {
                // A peer that closes without close_notify (our servers use Connection:
                // close) has still sent everything.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn host_of(authority: &str) -> &str {
    authority.rsplit_once(':').map_or(authority, |(h, _)| h).trim_matches(['[', ']'])
}

/// Wraps `tcp` for `base` (`http://` or `https://host:port`).
pub fn connect(base: &str, authority: &str, tcp: TcpStream) -> Result<Stream> {
    if base.starts_with("http://") {
        return Ok(Stream::Plain(tcp));
    }
    if let (Some((id, host_named)), Some((_, roots))) = (expected_node(authority), DEFAULT_TRUST.get()) {
        let (cfg, name) = node_config(authority, &id, host_named, roots.clone())?;
        let conn = rustls::ClientConnection::new(cfg, name)?;
        return Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))));
    }
    let cfg = trusted(authority)
        .ok_or_else(|| anyhow!("no CA for {authority}: add the remote with --ca-fingerprint (rekcod remote add NAME {base} --token T --ca-fingerprint sha256:...)"))?;
    let name = ServerName::try_from(host_of(authority).to_string()).map_err(|e| anyhow!("{authority}: {e}"))?;
    let conn = rustls::ClientConnection::new(cfg, name)?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// A server connection, plain or TLS, with the peer address read before wrapping.
pub enum ServerStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

pub fn accept(cfg: Option<&Arc<rustls::ServerConfig>>, tcp: TcpStream) -> Result<ServerStream> {
    Ok(match cfg {
        None => ServerStream::Plain(tcp),
        Some(cfg) => ServerStream::Tls(Box::new(rustls::StreamOwned::new(rustls::ServerConnection::new(cfg.clone())?, tcp))),
    })
}

impl Read for ServerStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ServerStream::Plain(s) => s.read(buf),
            ServerStream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for ServerStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            ServerStream::Plain(s) => s.write(buf),
            ServerStream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            ServerStream::Plain(s) => s.flush(),
            ServerStream::Tls(s) => s.flush(),
        }
    }
}

/// close_notify goes once, when the response is done: streamed replies (exec output) flush
/// after every chunk and must stay open.
impl Drop for ServerStream {
    fn drop(&mut self) {
        // Mid-handshake there's nothing to close, and completing it could wait on a dead peer.
        if let ServerStream::Tls(s) = self
            && !s.conn.is_handshaking()
        {
            s.conn.send_close_notify();
            while s.conn.wants_write() {
                if s.conn.write_tls(&mut s.sock).is_err() {
                    break;
                }
            }
        }
    }
}

/// Accepts any certificate: only for fetching `/ca.pem` before we know the CA, and the
/// result is checked against the fingerprint before anything secret is sent.
#[derive(Debug)]
struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Fetches `base`'s `/ca.pem` and returns it if it is one certificate whose fingerprint is
/// `want`. What comes back is that certificate encoded again, never the bytes received.
pub fn fetch_ca(base: &str, want: &str) -> Result<String> {
    let authority = base.strip_prefix("https://").ok_or_else(|| anyhow!("--ca-fingerprint is for https:// URLs"))?.trim_end_matches('/');
    let p = provider();
    let cfg = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCert(p)))
        .with_no_client_auth();
    let addr = crate::bonjour::socket_addr(authority)?;
    let tcp = TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5))?;
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let name = ServerName::try_from(host_of(authority).to_string()).map_err(|e| anyhow!("{authority}: {e}"))?;
    let mut s = rustls::StreamOwned::new(rustls::ClientConnection::new(Arc::new(cfg), name)?, tcp);
    write!(s, "GET /ca.pem HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")?;
    // A CA certificate is about 600 bytes; nothing here needs more than 64 KiB.
    let mut buf = Vec::new();
    match (&mut s).take(64 << 10).read_to_end(&mut buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(e.into()),
    }
    let text = String::from_utf8_lossy(&buf);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).context("bad /ca.pem response")?;
    let der = ca_cert(body).with_context(|| format!("{base}/ca.pem"))?;
    let got = der_fingerprint(&der);
    if !got.eq_ignore_ascii_case(want.trim()) {
        bail!("{base} presents CA {got}, not {want}; not trusting it");
    }
    Ok(pem_of(&der))
}

/// The node CA every test that dials a node through `connect` shares: `trust_by_default`
/// takes the first CA it's given for the life of the process.
#[cfg(test)]
pub(crate) fn test_node_ca() -> Ca {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("rekcod-test-node-ca-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        trust_by_default(&ensure_node_ca(&d).unwrap().pem).unwrap();
        d
    });
    ensure_node_ca(dir).unwrap()
}

/// Reads a request head. A test server must read all of it: closing with some unread resets
/// the connection before the client reads the reply.
#[cfg(test)]
fn read_head(s: ServerStream) -> std::io::BufReader<ServerStream> {
    let mut r = std::io::BufReader::new(s);
    let mut line = String::new();
    while std::io::BufRead::read_line(&mut r, &mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
        line.clear();
    }
    r
}

/// Serves `cert` on a listener; each connection reads a request and gets `200 {}`.
#[cfg(test)]
pub(crate) fn test_https_server(l: std::net::TcpListener, cert: &str, key: &str) {
    let server = server_config(cert, key).unwrap();
    std::thread::spawn(move || {
        for tcp in l.incoming().flatten() {
            let Ok(s) = accept(Some(&server), tcp) else { continue };
            let _ = read_head(s).get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_certs_serve_and_verify() {
        let dir = std::env::temp_dir().join(format!("rekcod-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca = ensure_ca(&dir).unwrap();
        // Loading it again gives the same CA.
        let again = ensure_ca(&dir).unwrap();
        assert_eq!(ca.fingerprint().unwrap(), again.fingerprint().unwrap());
        let (cert, key) = again.issue(&["127.0.0.1".into(), "localhost".into()]).unwrap();
        let server = server_config(&cert, &key).unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let ca_pem = ca.pem.clone();
        std::thread::spawn(move || {
            for tcp in l.incoming().flatten() {
                let Ok(mut s) = accept(Some(&server), tcp) else { continue };
                let mut req = [0u8; 4096];
                let n = s.read(&mut req).unwrap_or(0);
                let body = if String::from_utf8_lossy(&req[..n]).starts_with("GET /ca.pem") { ca_pem.clone() } else { "hello".into() };
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.flush();
            }
        });
        let base = format!("https://{addr}");
        // The CA comes back only with the right fingerprint.
        assert!(fetch_ca(&base, "sha256:00").is_err());
        let pem = fetch_ca(&base, &ca.fingerprint().unwrap()).unwrap();
        trust(&addr.to_string(), &pem).unwrap();
        let tcp = TcpStream::connect(addr).unwrap();
        let mut s = connect(&base, &addr.to_string(), tcp).unwrap();
        write!(s, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.ends_with("hello"), "{out}");
        // A CA we don't hold for that host: no connection. (Any socket will do; it's refused
        // before a byte is sent.)
        let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(unused.local_addr().unwrap()).unwrap();
        if DEFAULT_TRUST.get().is_none() {
            assert!(connect("https://10.9.9.9:1", "10.9.9.9:1", tcp).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real CA followed by another one at `/ca.pem`, with a server certificate from the
    /// other: the pin matches the first certificate, and the second must not be trusted.
    #[test]
    fn a_ca_pin_is_one_certificate() {
        let dir = std::env::temp_dir().join(format!("rekcod-tls-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("other")).unwrap();
        let (real, other) = (ensure_ca(&dir).unwrap(), ensure_ca(&dir.join("other")).unwrap());
        let bundle = format!("{}{}", real.pem, other.pem);
        let (cert, key) = other.issue(&["127.0.0.1".into()]).unwrap();
        let server = server_config(&cert, &key).unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let body = bundle.clone();
        std::thread::spawn(move || {
            for tcp in l.incoming().flatten() {
                let Ok(s) = accept(Some(&server), tcp) else { continue };
                let _ =
                    write!(read_head(s).get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let err = fetch_ca(&format!("https://{addr}"), &real.fingerprint().unwrap()).unwrap_err();
        assert!(format!("{err:#}").contains("2 certificates"), "{err:#}");
        // A bundle saved before this check isn't trusted, and has no fingerprint.
        assert!(trust("127.0.0.1:1", &bundle).is_err());
        assert!(fingerprint(&bundle).is_err());
        // One certificate encoded again is the same certificate.
        assert_eq!(fingerprint(&pem_of(&ca_cert(&real.pem).unwrap())).unwrap(), real.fingerprint().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A flush mid-response (exec output, one per chunk) must not end the stream.
    #[test]
    fn a_flush_keeps_the_response_open() {
        let dir = std::env::temp_dir().join(format!("rekcod-tls-flush-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca = ensure_ca(&dir).unwrap();
        let (cert, key) = ca.issue(&["127.0.0.1".into()]).unwrap();
        let server = server_config(&cert, &key).unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut s = accept(Some(&server), l.accept().unwrap().0).unwrap();
            let _ = s.read(&mut [0u8; 64]);
            for part in [b"a", b"b"] {
                s.write_all(part).unwrap();
                s.flush().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
        trust(&addr.to_string(), &ca.pem).unwrap();
        let mut s = connect(&format!("https://{addr}"), &addr.to_string(), TcpStream::connect(addr).unwrap()).unwrap();
        s.write_all(b"go").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert_eq!(out, "ab");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Serves one TLS handshake with `names` per connection; returns the address.
    fn node_server(ca: &Ca, names: &[String]) -> std::net::SocketAddr {
        let (cert, key) = ca.issue(names).unwrap();
        let server = server_config(&cert, &key).unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for tcp in l.incoming().flatten() {
                let Ok(mut s) = accept(Some(&server), tcp) else { continue };
                let _ = s.write_all(b"ok");
                let _ = s.flush();
            }
        });
        addr
    }

    fn handshake(addr: std::net::SocketAddr, id: &str, host_named: bool, roots: &Arc<rustls::RootCertStore>) -> bool {
        let (cfg, name) = node_config(&addr.to_string(), id, host_named, roots.clone()).unwrap();
        let mut s = rustls::StreamOwned::new(rustls::ClientConnection::new(cfg, name).unwrap(), TcpStream::connect(addr).unwrap());
        let mut b = [0u8; 2];
        s.read_exact(&mut b).is_ok()
    }

    #[test]
    fn node_certificates_are_bound_to_the_node_not_its_host() {
        let dir = std::env::temp_dir().join(format!("rekcod-tls-nodes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca = ensure_node_ca(&dir).unwrap();
        let roots = roots(&ca.pem).unwrap();
        // Two nodes on one host: each certificate passes only for its own node.
        let a = node_server(&ca, &[node_name("n-aaaa")]);
        let b = node_server(&ca, &[node_name("n-bbbb")]);
        assert!(handshake(a, "n-aaaa", false, &roots));
        assert!(handshake(b, "n-bbbb", false, &roots));
        assert!(!handshake(b, "n-aaaa", false, &roots), "n-bbbb's certificate must not pass for n-aaaa");
        // A certificate naming the host (how nodes joined before) passes only for a node
        // recorded as joined then.
        let old = node_server(&ca, &["127.0.0.1".into()]);
        assert!(!handshake(old, "n-aaaa", false, &roots));
        assert!(handshake(old, "n-aaaa", true, &roots));
        assert!(!handshake(b, "n-aaaa", true, &roots), "a host-named fallback still needs the host in the certificate");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
