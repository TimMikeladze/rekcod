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

/// `sha256:<hex>` of the first certificate in `pem`.
pub fn fingerprint(pem: &str) -> Result<String> {
    use sha2::Digest;
    let der = CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| anyhow!("not a certificate: {e}"))?;
    Ok(format!("sha256:{}", sha2::Sha256::digest(der.as_ref()).iter().map(|b| format!("{b:02x}")).collect::<String>()))
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

fn client_config(ca_pem: &str) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c.map_err(|e| anyhow!("CA: {e}"))?)?;
    }
    let cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// CAs to trust, by `host:port`, plus one for any other https URL (a controller trusts its
/// own CA for every node; a node trusts its controller's).
static TRUST: Mutex<Option<HashMap<String, Arc<rustls::ClientConfig>>>> = Mutex::new(None);
static DEFAULT_TRUST: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();

pub fn trust(authority: &str, ca_pem: &str) -> Result<()> {
    let cfg = client_config(ca_pem)?;
    TRUST.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_default().insert(authority.to_string(), cfg);
    Ok(())
}

pub fn trust_by_default(ca_pem: &str) -> Result<()> {
    let _ = DEFAULT_TRUST.set(client_config(ca_pem)?);
    Ok(())
}

fn trusted(authority: &str) -> Option<Arc<rustls::ClientConfig>> {
    let t = TRUST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    t.as_ref().and_then(|m| m.get(authority).cloned()).or_else(|| DEFAULT_TRUST.get().cloned())
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
            ServerStream::Tls(s) => {
                s.flush()?;
                // Send close_notify with the end of the response.
                s.conn.send_close_notify();
                s.conn.complete_io(&mut s.sock).map(|_| ())
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

/// Fetches `base`'s `/ca.pem` and returns it if its fingerprint is `want`.
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
    let mut buf = Vec::new();
    match s.read_to_end(&mut buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(e.into()),
    }
    let text = String::from_utf8_lossy(&buf);
    let pem = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).context("bad /ca.pem response")?;
    let got = fingerprint(&pem)?;
    if !got.eq_ignore_ascii_case(want.trim()) {
        bail!("{base} presents CA {got}, not {want}; not trusting it");
    }
    Ok(pem)
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
        let spare = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(spare.local_addr().unwrap()).unwrap();
        if DEFAULT_TRUST.get().is_none() {
            assert!(connect("https://10.9.9.9:1", "10.9.9.9:1", tcp).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
