//! `rekcod state`: everything the front ends show, with each item's actions attached, as one
//! JSON document. The dashboard (`dashboard/`, docs/dashboard.md) renders it as is and only
//! runs the argv it finds here.

use serde_json::{Value, json};

use crate::actions::{self, Action, Model, Run};
use crate::paths::Paths;

fn action_json(a: &Action) -> Value {
    let mut v = json!({
        "kind": format!("{:?}", a.kind),
        "label": a.label,
        "interactive": a.interactive,
        "confirm": a.confirm,
    });
    match &a.run {
        Run::Rekcod(argv) => v["argv"] = json!(argv),
        Run::Open(url) => v["url"] = json!(url),
    }
    if let Some(dir) = &a.dir {
        v["dir"] = json!(dir);
    }
    v
}

fn actions_json(list: &[Action]) -> Value {
    Value::Array(list.iter().map(action_json).collect())
}

/// `ps` CPU % and resident MiB for each pid, in one call.
fn proc_usage(pids: &[i32]) -> std::collections::HashMap<i32, (f64, u64)> {
    if pids.is_empty() {
        return Default::default();
    }
    let list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let out = std::process::Command::new("/bin/ps").args(["-o", "pid=,%cpu=,rss=", "-p", &list]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.parse().ok()?, (f.next()?.parse().ok()?, f.next()?.parse::<u64>().ok()? / 1024)))
        })
        .collect()
}

/// Seconds since `pid` started, from `ps -o etime=` (`[[dd-]hh:]mm:ss`).
fn uptime_secs(pid: i64) -> Option<u64> {
    let out = std::process::Command::new("/bin/ps").args(["-o", "etime=", "-p", &pid.to_string()]).output().ok()?;
    parse_etime(String::from_utf8_lossy(&out.stdout).trim())
}

fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let secs = rest.split(':').try_fold(0u64, |acc, p| p.parse::<u64>().ok().map(|n| acc * 60 + n))?;
    Some(days * 86_400 + secs)
}

/// The model plus actions, without anything that needs the network beyond what `Model::load` reads.
pub fn model_json(m: &Model) -> Value {
    let containers: Vec<Value> = m
        .containers
        .iter()
        .map(|c| {
            json!({
                "id": c.id, "name": c.name, "image": c.image, "state": c.state, "status": c.status,
                "project": c.project, "service": c.service, "ports": c.ports, "volumes": c.volumes,
                "actions": actions_json(&actions::container_actions(c)),
            })
        })
        .collect();
    let ports: Vec<Value> = m
        .ports
        .iter()
        .map(|p| {
            json!({
                "port": p.port, "udp": p.udp, "container": p.container, "error": p.error, "sandbox": p.sandbox,
                "actions": actions_json(&actions::port_actions(p)),
            })
        })
        .collect();
    let vol_size = |name: &str| m.volumes.iter().find(|v| v.name == name).and_then(|v| v.size);
    let projects: Vec<Value> = m
        .projects
        .iter()
        .map(|p| {
            let snapshots: Vec<Value> = p
                .snapshots
                .iter()
                .map(|s| {
                    let sizes: Option<u64> = s.volume_names.iter().map(|v| vol_size(v)).sum();
                    json!({"name": s.name, "created": s.created, "volumes": s.volumes, "size": sizes,
                           "current": p.branch.as_deref() == Some(s.name.as_str()),
                           "actions": actions_json(&actions::snapshot_actions(s))})
                })
                .collect();
            json!({"name": p.name, "dir": p.dir, "containers": p.containers, "running": p.running, "branch": p.branch,
                   "snapshots": snapshots, "actions": actions_json(&actions::project_actions(p))})
        })
        .collect();
    let usage = proc_usage(&m.sandboxes.iter().filter_map(|s| s.pid).collect::<Vec<_>>());
    let sandboxes: Vec<Value> = m
        .sandboxes
        .iter()
        .map(|s| {
            let u = s.pid.and_then(|p| usage.get(&p));
            json!({
                "id": s.id, "name": s.name, "project": s.project, "copy": s.copy, "running": s.running, "macos": s.macos,
                "created": s.created, "cpus": s.cpus, "memory_gib": s.memory_gib,
                "cpu_pct": u.map(|u| u.0), "rss_mib": u.map(|u| u.1),
                "actions": actions_json(&actions::sandbox_actions(s)),
            })
        })
        .collect();
    let apps: Vec<Value> = m
        .apps
        .iter()
        .map(|a| {
            json!({
                "name": a.name, "url": a.url, "live_release": a.live_release, "public": a.public, "sleeping": a.sleeping,
                "actions": actions_json(&actions::app_actions(a)),
            })
        })
        .collect();
    let jobs: Vec<Value> = m
        .jobs
        .iter()
        .map(|j| {
            json!({
                "id": j.id, "name": j.name, "state": j.state, "exit": j.exit, "created": j.created,
                "actions": actions_json(&actions::job_actions(j)),
            })
        })
        .collect();
    let images: Vec<Value> = m
        .images
        .iter()
        .map(|i| {
            json!({"id": i.id, "tags": i.tags, "size": i.size, "created": i.created, "containers": i.containers,
                   "dangling": i.dangling, "actions": actions_json(&actions::image_actions(i))})
        })
        .collect();
    let volumes: Vec<Value> = m
        .volumes
        .iter()
        .map(|v| {
            json!({"name": v.name, "driver": v.driver, "size": v.size, "containers": v.containers, "anonymous": v.anonymous,
                   "snapshot": v.snapshot, "actions": actions_json(&actions::volume_actions(v))})
        })
        .collect();
    let nodes: Option<Vec<Value>> = m.nodes.as_ref().map(|n| {
        n.iter()
            .map(|n| json!({"id": n.id, "state": n.state, "last_seen": n.last_seen, "actions": actions_json(&actions::node_actions(n))}))
            .collect()
    });
    let hosts: Vec<Value> = m.hosts.all.iter().map(|h| json!({"name": h.name, "endpoint": h.endpoint, "current": h.current})).collect();
    let mut status = m.status.clone();
    if let Some(up) = status["pid"].as_i64().and_then(uptime_secs) {
        status["uptime_secs"] = up.into();
    }
    json!({
        "running": m.running,
        "status": status,
        "sandbox_vm": m.sandbox,
        "socket_installed": m.socket_installed,
        "autostart": m.autostart,
        "vm_actions": actions_json(&actions::vm_actions(m.running, m.socket_installed, m.autostart)),
        "storage_actions": actions_json(&actions::storage_actions(m.running, &m.containers)),
        "containers": containers,
        "ports": ports,
        "projects": projects,
        "sandboxes": sandboxes,
        "images": images,
        "volumes": volumes,
        "disk": m.disk.as_ref().map(|d| json!({
            "checked": d.checked, "images": d.images, "containers": d.containers, "volumes": d.volumes,
            "build_cache": d.build_cache, "build_cache_free": d.build_cache_free,
        })),
        "disk_actions": actions_json(&actions::disk_actions(m.running, &m.images, &m.volumes, m.disk.as_ref())),
        "apps": apps,
        "jobs": jobs,
        "nodes": nodes,
        "namespaces": {"current": m.namespaces.current, "all": m.namespaces.all, "detail": m.namespaces.detail,
                       "actions": actions_json(&actions::namespace_actions(&m.namespaces))},
        "hosts": {"all": hosts, "local_current": m.hosts.local_current, "actions": actions_json(&actions::host_actions(&m.hosts))},
        "bans": m.bans.as_ref().map(|b| json!({"nets": b, "clients": m.edge_clients.iter().map(|c| json!({"ip": c.ip, "requests": c.requests, "limited": c.limited})).collect::<Vec<_>>(),
                                                "actions": actions_json(&actions::ban_actions(b, &m.edge_clients))})),
        "remotes": {"default": m.remotes.default, "all": m.remotes.all, "actions": actions_json(&actions::remote_actions(&m.remotes))},
        "controller_actions": actions_json(&actions::controller_actions(m.admin)),
    })
}

/// Fleet detail the menu bar has no room for: per-app releases, instances and traffic, full
/// node and job rows, schedules, and the admin views (routes, backups, ban detail). Read-only.
/// Each call that fails adds to `remote.errors`.
fn fleet_json(paths: &Paths, m: &Model, out: &mut Value) {
    let remotes = crate::fleet::remotes(paths);
    let Ok(r) = crate::fleet::remote(paths, None) else {
        out["remote"] = Value::Null;
        out["schedules"] = json!([]);
        return;
    };
    let mut errors: serde_json::Map<String, Value> = m.remote_errors.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let down = m.remote_errors.values().any(|e| actions::unreachable(e));
    let mut get = |what: &str, path: &str| -> Option<Value> {
        if down {
            return None;
        }
        match crate::fleet::call(&r.url, "GET", path, Some(&r.token), &Value::Null) {
            Ok(v) => Some(v),
            Err(e) if actions::admin_only(&e) => None,
            Err(e) => {
                errors.insert(what.into(), json!(format!("{e:#}")));
                None
            }
        }
    };
    // One call for every app's detail; older controllers ignore `detail` and get one call each.
    if let Some(list) = get("apps", &r.scoped("/apps?detail=1")) {
        let detail: std::collections::HashMap<String, Value> = list
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a.get("detail").is_some())
            .map(|a| (a["name"].as_str().unwrap_or_default().to_string(), a["detail"].clone()))
            .collect();
        if let Some(apps) = out["apps"].as_array_mut() {
            for a in apps {
                let name = a["name"].as_str().unwrap_or_default().to_string();
                a["detail"] = match detail.get(&name) {
                    Some(d) => d.clone(),
                    None => get("apps", &r.scoped(format!("/apps/{name}"))).unwrap_or(Value::Null),
                };
            }
        }
    }
    if m.admin {
        if let (Some(full), Some(nodes)) = (get("nodes", "/nodes"), out["nodes"].as_array_mut()) {
            for n in nodes {
                if let Some(f) = full.as_array().into_iter().flatten().find(|f| f["id"] == n["id"]) {
                    let actions = n["actions"].take();
                    *n = f.clone();
                    n["actions"] = actions;
                }
            }
        }
        out["routes"] = get("routes", "/routes?peek=1").unwrap_or(Value::Null);
        // A controller without --storage has no backups; that's shown in the view, not as a remote error.
        out["backups"] = match crate::fleet::call(&r.url, "GET", "/backups", Some(&r.token), &Value::Null) {
            Ok(v) => json!({"list": v}),
            Err(e) => json!({"error": format!("{e:#}")}),
        };
        if let (Some(b), true) = (get("bans", "/bans"), out["bans"].is_object()) {
            out["bans"]["detail"] = b;
        }
    }
    // The full job rows (command, node, finished) rather than the menu bar's ten summaries.
    if let Some(v) = get("jobs", &r.scoped("/jobs")) {
        let jobs: Vec<Value> = actions::parse_jobs(&v, 50)
            .iter()
            .zip(v["jobs"].as_array().into_iter().flatten())
            .map(|(summary, j)| {
                let mut j = j.clone();
                j["actions"] = actions_json(&actions::job_actions(summary));
                j
            })
            .collect();
        out["jobs"] = json!(jobs);
        out["schedules"] = v["schedules"].clone();
    } else {
        out["schedules"] = json!([]);
    }
    let first = errors.values().next().cloned();
    out["remote"] = json!({"name": remotes.default, "url": r.url, "ns": r.ns(), "all": remotes.remotes.keys().collect::<Vec<_>>(),
                           "admin": m.admin, "error": first, "errors": errors});
}

/// `local`: no controller calls (the dashboard's refresh after a docker event); `remote` and
/// the fleet lists are left out and `"local": true` says so.
pub fn print(paths: &Paths, local: bool) -> anyhow::Result<()> {
    let m = Model::load_with(paths, !local);
    let mut v = model_json(&m);
    if local {
        v["local"] = json!(true);
    } else {
        fleet_json(paths, &m, &mut v);
    }
    v["generated"] = json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs());
    println!("{}", serde_json::to_string(&v)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::tests::*;

    /// Every kind the menu bar offers comes out of `rekcod state` too, so the dashboard has them all.
    #[test]
    fn state_offers_every_action_kind() {
        let (containers, ports, projects) = sample();
        let mut seen = std::collections::BTreeSet::new();
        let mut collect = |v: &Value| {
            fn walk(v: &Value, seen: &mut std::collections::BTreeSet<String>) {
                match v {
                    Value::Object(o) => {
                        if let (Some(k), true) = (o.get("kind").and_then(Value::as_str), o.contains_key("label")) {
                            seen.insert(k.to_string());
                        }
                        o.values().for_each(|x| walk(x, seen));
                    }
                    Value::Array(a) => a.iter().for_each(|x| walk(x, seen)),
                    _ => {}
                }
            }
            walk(v, &mut seen);
        };
        let full = Model {
            running: true,
            containers: containers.clone(),
            ports,
            projects,
            sandboxes: vec![sample_sandbox()],
            apps: vec![sample_app(), actions::RemoteApp { public: true, sleeping: true, ..sample_app() }],
            namespaces: sample_namespaces(),
            hosts: sample_hosts(),
            bans: Some(sample_bans()),
            images: sample_images(),
            volumes: sample_volumes(),
            nodes: Some(sample_nodes()),
            edge_clients: sample_clients(),
            remotes: sample_remotes(),
            admin: true,
            jobs: vec![sample_job(), actions::RemoteJob { state: "done".into(), ..sample_job() }],
            ..Default::default()
        };
        collect(&model_json(&full));
        collect(&model_json(&Model { socket_installed: false, ..Default::default() }));
        collect(&model_json(&Model { running: true, autostart: true, containers: vec![sample_storage()], ..Default::default() }));
        for kind in actions::Kind::ALL {
            assert!(seen.contains(&format!("{kind:?}")), "{kind:?} is missing from `rekcod state`");
        }
        let c = &model_json(&full)["containers"][0]["actions"][0];
        assert_eq!(c["argv"], json!(["stop", "web"]));
    }

    #[test]
    fn etime_parses() {
        assert_eq!(parse_etime("05:07"), Some(307));
        assert_eq!(parse_etime("02:05:07"), Some(7507));
        assert_eq!(parse_etime("3-02:05:07"), Some(3 * 86_400 + 7507));
        assert_eq!(parse_etime(""), None);
    }
}
