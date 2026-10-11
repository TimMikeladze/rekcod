//! Which Mac addresses to listen on for the guest's published ports.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Binding {
    pub addr: IpAddr,
    pub port: u16,
}

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Ports", default)]
    ports: Option<Vec<Port>>,
}

#[derive(Deserialize)]
struct Port {
    #[serde(rename = "IP", default)]
    ip: String,
    #[serde(rename = "PublicPort")]
    public_port: Option<u16>,
    #[serde(rename = "Type")]
    kind: String,
}

/// Docker's `HostIp` mapped to a Mac listen address. Wildcards (v4 or v6)
/// become one dual-stack `[::]` listener so `localhost` works for both families.
pub fn mac_addr(host_ip: &str) -> Option<IpAddr> {
    match host_ip {
        "" | "0.0.0.0" | "::" => Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
        other => other.parse().ok(),
    }
}

/// Desired listeners from a `GET /containers/json` body: binding -> container name.
pub fn desired(containers_json: &[u8]) -> serde_json::Result<BTreeMap<Binding, String>> {
    let containers: Vec<Container> = serde_json::from_slice(containers_json)?;
    let mut out = BTreeMap::new();
    for c in containers {
        let name = c.names.first().map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default();
        for p in c.ports.unwrap_or_default() {
            let (Some(port), "tcp") = (p.public_port, p.kind.as_str()) else { continue };
            if let Some(addr) = mac_addr(&p.ip) {
                out.insert(Binding { addr, port }, name.clone());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_collapse_to_one_dual_stack_listener() {
        let json = br#"[{"Names":["/web-1"],"Ports":[
            {"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8080,"Type":"tcp"},
            {"IP":"::","PrivatePort":80,"PublicPort":8080,"Type":"tcp"},
            {"IP":"127.0.0.1","PrivatePort":5432,"PublicPort":5432,"Type":"tcp"},
            {"PrivatePort":9000,"Type":"tcp"},
            {"IP":"0.0.0.0","PrivatePort":53,"PublicPort":53,"Type":"udp"}]}]"#;
        let d = desired(json).unwrap();
        let want: Vec<(String, u16)> = vec![("127.0.0.1".into(), 5432), ("::".into(), 8080)];
        let got: Vec<(String, u16)> = d.keys().map(|b| (b.addr.to_string(), b.port)).collect();
        assert_eq!(got, want);
        assert!(d.values().all(|n| n == "web-1"));
    }

    #[test]
    fn empty_and_unknown_fields_are_fine() {
        assert!(desired(b"[]").unwrap().is_empty());
        assert!(desired(br#"[{"Id":"x","Ports":null}]"#).unwrap().is_empty());
        assert_eq!(mac_addr("not-an-ip"), None);
    }
}
