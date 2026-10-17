//! A server on a LAN found by identity instead of address (docs/lan-discovery.md).
//!
//! The controller registers `rekcod-<fingerprint>` as `_rekcod._tcp`; a remote URL
//! `https://rekcod-<fingerprint>.bonjour` is resolved through mDNSResponder on every call.

use std::ffi::{CStr, CString, c_char, c_void};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

const TYPE: &str = "_rekcod._tcp";
/// The pseudo top-level domain of a remote found over Bonjour.
pub const SUFFIX: &str = ".bonjour";

type Ref = *mut c_void;
type ResolveReply = extern "C" fn(Ref, u32, u32, i32, *const c_char, *const c_char, u16, u16, *const u8, *mut c_void);

#[link(name = "System")]
unsafe extern "C" {
    fn DNSServiceRegister(
        sd: *mut Ref,
        flags: u32,
        interface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt: *const c_void,
        cb: *const c_void,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        sd: *mut Ref,
        flags: u32,
        interface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        cb: ResolveReply,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd: Ref) -> i32;
    fn DNSServiceProcessResult(sd: Ref) -> i32;
    fn DNSServiceRefDeallocate(sd: Ref);
}

/// `rekcod-` and the first 12 hex digits of a `sha256:...` fingerprint.
pub fn instance(fingerprint: &str) -> String {
    let hex: String = fingerprint.trim_start_matches("sha256:").chars().filter(char::is_ascii_hexdigit).take(12).collect();
    format!("rekcod-{}", hex.to_ascii_lowercase())
}

/// The instance in `rekcod-x.bonjour[:port]`, if that's what `authority` is.
pub fn instance_of(authority: &str) -> Option<&str> {
    let host = authority.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()).map_or(authority, |(h, _)| h);
    host.strip_suffix(SUFFIX).filter(|i| !i.is_empty())
}

/// Registers the instance until the process exits (mDNSResponder drops it when our socket closes).
pub fn advertise(instance: &str, port: u16) -> Result<()> {
    let (name, ty) = (CString::new(instance)?, CString::new(TYPE)?);
    let mut sd: Ref = std::ptr::null_mut();
    let err = unsafe {
        DNSServiceRegister(
            &mut sd,
            0,
            0,
            name.as_ptr(),
            ty.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            port.to_be(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if err != 0 {
        bail!("Bonjour register {instance}: error {err}");
    }
    // Never deallocated: the registration lives as long as the controller.
    Ok(())
}

struct Found {
    host: Option<String>,
    port: u16,
}

extern "C" fn on_resolve(
    _: Ref,
    _: u32,
    _: u32,
    err: i32,
    _: *const c_char,
    host: *const c_char,
    port: u16,
    _: u16,
    _: *const u8,
    ctx: *mut c_void,
) {
    let found = unsafe { &mut *ctx.cast::<Found>() };
    if err == 0 && !host.is_null() {
        found.host = Some(unsafe { CStr::from_ptr(host) }.to_string_lossy().trim_end_matches('.').to_string());
        found.port = u16::from_be(port);
    }
}

/// The instance's current host name and port.
pub fn resolve(instance: &str, timeout: Duration) -> Result<(String, u16)> {
    let (name, ty, domain) = (CString::new(instance)?, CString::new(TYPE)?, CString::new("local.")?);
    let mut found = Found { host: None, port: 0 };
    let mut sd: Ref = std::ptr::null_mut();
    let err = unsafe { DNSServiceResolve(&mut sd, 0, 0, name.as_ptr(), ty.as_ptr(), domain.as_ptr(), on_resolve, (&raw mut found).cast()) };
    if err != 0 {
        bail!("Bonjour resolve {instance}: error {err}");
    }
    let fd = unsafe { DNSServiceRefSockFD(sd) };
    let deadline = Instant::now() + timeout;
    while found.host.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut p, 1, left.as_millis().min(i32::MAX as u128) as i32) } <= 0 {
            break;
        }
        if unsafe { DNSServiceProcessResult(sd) } != 0 {
            break;
        }
    }
    unsafe { DNSServiceRefDeallocate(sd) };
    found.host.map(|h| (h, found.port)).ok_or_else(|| {
        anyhow!("{instance} isn't answering on Bonjour; is the server on, and on the same network (mDNS doesn't cross subnets)?")
    })
}

/// Where to connect for `authority`: a Bonjour instance resolved now, else ordinary DNS.
/// IPv4 first, since a link-local IPv6 answer needs a scope this socket doesn't carry.
pub fn socket_addr(authority: &str) -> Result<SocketAddr> {
    let addrs: Vec<SocketAddr> = match instance_of(authority) {
        Some(i) => {
            let (host, port) = resolve(i, Duration::from_secs(3))?;
            (host.as_str(), port).to_socket_addrs()?.collect()
        }
        None => authority.to_socket_addrs()?.collect(),
    };
    addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()).copied().ok_or_else(|| anyhow!("can't resolve {authority}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(instance("sha256:3F9A1C2B7D4E55aa"), "rekcod-3f9a1c2b7d4e");
        assert_eq!(instance_of("rekcod-3f9a1c2b7d4e.bonjour"), Some("rekcod-3f9a1c2b7d4e"));
        assert_eq!(instance_of("rekcod-3f9a1c2b7d4e.bonjour:7700"), Some("rekcod-3f9a1c2b7d4e"));
        assert_eq!(instance_of("mini.local:7700"), None);
        assert_eq!(instance_of(".bonjour"), None);
    }

    /// Registers a throwaway instance and finds it again through mDNSResponder.
    #[test]
    fn advertise_then_resolve() {
        let name = format!("rekcod-test-{}", std::process::id());
        advertise(&name, 54321).unwrap();
        let (host, port) = resolve(&name, Duration::from_secs(5)).unwrap();
        assert_eq!(port, 54321);
        assert!(host.ends_with(".local"), "{host}");
        assert!(socket_addr(&format!("{name}.bonjour")).unwrap().port() == 54321);
    }
}
