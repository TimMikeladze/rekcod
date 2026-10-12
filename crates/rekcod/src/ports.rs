//! Which Mac addresses to listen on for the guest's published ports.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr};

pub use rekcod_proto::Proto;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Binding {
    pub proto: Proto,
    pub addr: IpAddr,
    pub port: u16,
}

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Ports", default)]
    ports: Option<Vec<Port>>,
    /// Absent in old fixtures; `GET /containers/json?all=1` lists stopped ones too.
    #[serde(rename = "State", default)]
    state: Option<String>,
    #[serde(rename = "Id", default)]
    id: String,
    #[serde(rename = "Labels", default)]
    labels: Option<BTreeMap<String, String>>,
}

/// Containers that want their listening ports forwarded without `-p` (`rekcod x`).
pub const AUTOPORTS_LABEL: &str = "dev.rekcod.autoports";

/// Running containers labelled [`AUTOPORTS_LABEL`]: (id, name).
pub fn autoport_containers(containers_json: &[u8]) -> serde_json::Result<Vec<(String, String)>> {
    let containers: Vec<Container> = serde_json::from_slice(containers_json)?;
    Ok(containers
        .into_iter()
        .filter(|c| c.state.as_deref() == Some("running"))
        .filter(|c| c.labels.as_ref().is_some_and(|l| l.contains_key(AUTOPORTS_LABEL)))
        .map(|c| (c.id, c.names.first().map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default()))
        .collect())
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
    for c in containers.into_iter().filter(|c| c.state.as_deref().is_none_or(|s| s == "running")) {
        let name = c.names.first().map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default();
        for p in c.ports.unwrap_or_default() {
            let proto = match p.kind.as_str() {
                "tcp" => Proto::Tcp,
                "udp" => Proto::Udp,
                _ => continue,
            };
            let Some(port) = p.public_port else { continue };
            if let Some(addr) = mac_addr(&p.ip) {
                out.insert(Binding { proto, addr, port }, name.clone());
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
            {"IP":"0.0.0.0","PrivatePort":53,"PublicPort":5353,"Type":"udp"},
            {"IP":"::","PrivatePort":53,"PublicPort":5353,"Type":"udp"},
            {"IP":"0.0.0.0","PrivatePort":9,"PublicPort":9,"Type":"sctp"}]}]"#;
        let d = desired(json).unwrap();
        let want: Vec<(Proto, String, u16)> =
            vec![(Proto::Tcp, "127.0.0.1".into(), 5432), (Proto::Tcp, "::".into(), 8080), (Proto::Udp, "::".into(), 5353)];
        let got: Vec<(Proto, String, u16)> = d.keys().map(|b| (b.proto, b.addr.to_string(), b.port)).collect();
        assert_eq!(got, want);
        assert!(d.values().all(|n| n == "web-1"));
    }

    #[test]
    fn only_running_containers_publish() {
        let json = br#"[{"Names":["/a"],"State":"created","Ports":[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8080,"Type":"tcp"}]},
            {"Names":["/b"],"State":"running","Ports":[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8081,"Type":"tcp"}]}]"#;
        let ports: Vec<u16> = desired(json).unwrap().keys().map(|b| b.port).collect();
        assert_eq!(ports, vec![8081]);
    }

    #[test]
    fn autoport_containers_need_label_and_running() {
        let json = br#"[{"Id":"a1","Names":["/x"],"State":"running","Labels":{"dev.rekcod.autoports":"1"}},
            {"Id":"b2","Names":["/y"],"State":"exited","Labels":{"dev.rekcod.autoports":"1"}},
            {"Id":"c3","Names":["/z"],"State":"running","Labels":{}}]"#;
        assert_eq!(autoport_containers(json).unwrap(), vec![("a1".to_string(), "x".to_string())]);
    }

    #[test]
    fn empty_and_unknown_fields_are_fine() {
        assert!(desired(b"[]").unwrap().is_empty());
        assert!(desired(br#"[{"Id":"x","Ports":null}]"#).unwrap().is_empty());
        assert_eq!(mac_addr("not-an-ip"), None);
    }
}
