//! What the menu bar shows and can do (docs/frontends.md).
//!
//! The CLI is the implementation: every [`Action`] is a `rekcod` command line
//! (or opening a URL). The menu bar renders the lists returned by the
//! `*_actions` functions as they are, so an action added here appears there,
//! and `tests::every_kind_is_offered` fails until every [`Kind`] is reachable.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    VmStart,
    VmStop,
    Compact,
    Shell,
    ContainerStart,
    ContainerStop,
    ContainerRestart,
    ContainerLogs,
    ContainerRemove,
    OpenPort,
    SnapSave,
    SnapRestore,
    SnapRemove,
    SocketInstall,
}

impl Kind {
    /// Every kind; the parity tests check each one reaches the menu bar.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: [Kind; 14] = [
        Kind::VmStart,
        Kind::VmStop,
        Kind::Compact,
        Kind::Shell,
        Kind::ContainerStart,
        Kind::ContainerStop,
        Kind::ContainerRestart,
        Kind::ContainerLogs,
        Kind::ContainerRemove,
        Kind::OpenPort,
        Kind::SnapSave,
        Kind::SnapRestore,
        Kind::SnapRemove,
        Kind::SocketInstall,
    ];

}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// `rekcod <argv>`.
    Rekcod(Vec<String>),
    /// `open <url>`.
    Open(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub kind: Kind,
    pub label: String,
    pub run: Run,
    /// Working directory (snapshot names default to that directory's git branch).
    pub dir: Option<PathBuf>,
    /// Needs a terminal (shell, logs).
    pub interactive: bool,
    /// Ask before running (destroys or overwrites something). The question.
    pub confirm: Option<String>,
}

impl Action {
    fn rekcod(kind: Kind, label: impl Into<String>, argv: &[&str]) -> Action {
        Action {
            kind,
            label: label.into(),
            run: Run::Rekcod(argv.iter().map(|s| s.to_string()).collect()),
            dir: None,
            interactive: false,
            confirm: None,
        }
    }

    fn interactive(mut self) -> Action {
        self.interactive = true;
        self
    }

    fn confirm(mut self, question: String) -> Action {
        self.confirm = Some(question);
        self
    }

    /// The command line, for display and for Terminal.app.
    pub fn command_line(&self) -> String {
        match &self.run {
            Run::Rekcod(argv) => {
                let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "rekcod".into());
                std::iter::once(exe).chain(argv.iter().cloned()).map(|a| shell_quote(&a)).collect::<Vec<_>>().join(" ")
            }
            Run::Open(url) => format!("open {}", shell_quote(url)),
        }
    }

    /// A non-interactive command, ready to spawn with captured output.
    pub fn command(&self) -> Command {
        let mut cmd = match &self.run {
            Run::Rekcod(argv) => {
                let mut c = Command::new(std::env::current_exe().unwrap_or_else(|_| "rekcod".into()));
                c.args(argv);
                c
            }
            Run::Open(url) => {
                let mut c = Command::new("/usr/bin/open");
                c.arg(url);
                c
            }
        };
        if let Some(dir) = &self.dir {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null());
        cmd
    }
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

// ---------------------------------------------------------------- model

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub project: Option<String>,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub port: u16,
    pub udp: bool,
    pub container: String,
    pub error: Option<String>,
    pub sandbox: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    /// Where compose ran it (from container labels), if any container is left.
    pub dir: Option<PathBuf>,
    pub snapshots: Vec<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub project: String,
    pub name: String,
    pub created: u64,
    pub volumes: Vec<String>,
}

/// Everything the menu bar shows, loaded in one go.
#[derive(Debug, Default)]
pub struct Model {
    pub running: bool,
    /// `rekcod status --json` without the sandbox key.
    pub status: Value,
    pub containers: Vec<Container>,
    pub ports: Vec<Port>,
    pub projects: Vec<Project>,
    /// The sandbox VM's status, when it runs.
    pub sandbox: Option<Value>,
    /// `/var/run/docker.sock` goes through rekcod (`rekcod socket install` done).
    pub socket_installed: bool,
}

impl Model {
    pub fn load(paths: &Paths) -> Model {
        let status = crate::cli::status_value(paths);
        let running = status["running"] == true;
        let sandbox = crate::cli::sandbox_status(paths);
        let socket_installed = crate::syssock::installed(paths);
        let mut ports = ports_of(&status, false);
        if let Some(sb) = &sandbox {
            ports.extend(ports_of(sb, true));
        }
        if !running {
            return Model { running, status, ports, sandbox, socket_installed, ..Default::default() };
        }
        let raw = crate::cli::api_get(paths, "/containers/json?all=1").unwrap_or_default();
        let containers = parse_containers(&raw);
        let filter = "%7B%22label%22%3A%5B%22dev.rekcod.snap%22%5D%7D"; // {"label":["dev.rekcod.snap"]}
        let snapshots = crate::cli::api_get(paths, &format!("/volumes?filters={filter}"))
            .ok()
            .map(|b| parse_snapshots(&b))
            .unwrap_or_default();
        let projects = projects(&containers, &raw, snapshots);
        Model { running, status, containers, ports, projects, sandbox, socket_installed }
    }
}

fn ports_of(status: &Value, sandbox: bool) -> Vec<Port> {
    let mut out: Vec<Port> = Vec::new();
    for p in status["ports"].as_array().into_iter().flatten() {
        let Some(port) = p["port"].as_u64().and_then(|n| u16::try_from(n).ok()) else { continue };
        let udp = p["proto"].as_str() == Some("udp");
        // 127.0.0.1 + ::1 pairs (autoports) are one port to the user.
        if out.iter().any(|o| o.port == port && o.udp == udp) {
            continue;
        }
        out.push(Port {
            port,
            udp,
            container: p["container"].as_str().unwrap_or("?").to_string(),
            error: p["error"].as_str().map(String::from),
            sandbox,
        });
    }
    out
}

pub fn parse_containers(json: &[u8]) -> Vec<Container> {
    let list: Vec<Value> = serde_json::from_slice(json).unwrap_or_default();
    let mut out: Vec<Container> = list
        .iter()
        .map(|c| {
            let mut ports: Vec<u16> = c["Ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["PublicPort"].as_u64().and_then(|n| u16::try_from(n).ok()))
                .collect();
            ports.sort();
            ports.dedup();
            Container {
                id: c["Id"].as_str().unwrap_or_default().to_string(),
                name: c["Names"][0].as_str().unwrap_or("?").trim_start_matches('/').to_string(),
                image: c["Image"].as_str().unwrap_or("?").to_string(),
                state: c["State"].as_str().unwrap_or("?").to_string(),
                status: c["Status"].as_str().unwrap_or_default().to_string(),
                project: c["Labels"]["com.docker.compose.project"].as_str().map(String::from),
                ports,
            }
        })
        .collect();
    // Running first, then by name.
    out.sort_by(|a, b| (a.state != "running", &a.name).cmp(&(b.state != "running", &b.name)));
    out
}

/// Snapshots from `GET /volumes` (labels written by `rekcod snap save`).
pub fn parse_snapshots(json: &[u8]) -> Vec<Snapshot> {
    let v: Value = serde_json::from_slice(json).unwrap_or_default();
    let mut out: Vec<Snapshot> = Vec::new();
    for vol in v["Volumes"].as_array().into_iter().flatten() {
        let l = &vol["Labels"];
        let (Some(project), Some(name), Some(source)) =
            (l["dev.rekcod.snap.project"].as_str(), l["dev.rekcod.snap.name"].as_str(), l["dev.rekcod.snap.volume"].as_str())
        else {
            continue;
        };
        let created = l["dev.rekcod.snap.created"].as_str().and_then(|c| c.parse().ok()).unwrap_or(0);
        match out.iter_mut().find(|s| s.project == project && s.name == name) {
            Some(s) => {
                s.created = s.created.max(created);
                s.volumes.push(source.to_string());
                s.volumes.sort();
            }
            None => out.push(Snapshot { project: project.into(), name: name.into(), created, volumes: vec![source.into()] }),
        }
    }
    out.sort_by(|a, b| (&a.project, std::cmp::Reverse(a.created)).cmp(&(&b.project, std::cmp::Reverse(b.created))));
    out
}

/// Compose projects (from container labels) plus projects that only have snapshots.
fn projects(containers: &[Container], containers_json: &[u8], snapshots: Vec<Snapshot>) -> Vec<Project> {
    let raw: Vec<Value> = serde_json::from_slice(containers_json).unwrap_or_default();
    let mut out: Vec<Project> = Vec::new();
    for c in containers {
        let Some(name) = &c.project else { continue };
        if out.iter().any(|p| &p.name == name) {
            continue;
        }
        let dir = raw
            .iter()
            .find(|r| r["Id"].as_str() == Some(&c.id))
            .and_then(|r| r["Labels"]["com.docker.compose.project.working_dir"].as_str())
            .map(PathBuf::from);
        out.push(Project { name: name.clone(), dir, snapshots: Vec::new() });
    }
    for s in snapshots {
        match out.iter_mut().find(|p| p.name == s.project) {
            Some(p) => p.snapshots.push(s),
            None => out.push(Project { name: s.project.clone(), dir: None, snapshots: vec![s] }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// -------------------------------------------------------------- actions

pub fn vm_actions(running: bool, socket_installed: bool) -> Vec<Action> {
    let mut out = if !running {
        vec![Action::rekcod(Kind::VmStart, "Start rekcod", &["start"])]
    } else {
        vec![
            Action::rekcod(Kind::VmStop, "Stop rekcod", &["stop"])
                .confirm("Stop rekcod? Running containers stop (and `--rm` ones are deleted).".into()),
            Action::rekcod(Kind::Compact, "Compact data disk", &["compact"]),
            Action::rekcod(Kind::Shell, "Root shell in the VM", &["shell"]).interactive(),
        ]
    };
    if !socket_installed {
        // sudo asks for a password, so it runs in Terminal.
        out.push(
            Action::rekcod(Kind::SocketInstall, "Use rekcod for /var/run/docker.sock", &["socket", "install"]).interactive(),
        );
    }
    out
}

pub fn container_actions(c: &Container) -> Vec<Action> {
    let n = c.name.as_str();
    let mut out = Vec::new();
    if c.state == "running" {
        out.push(Action::rekcod(Kind::ContainerStop, "Stop", &["stop", n]));
        out.push(Action::rekcod(Kind::ContainerRestart, "Restart", &["restart", n]));
    } else {
        out.push(Action::rekcod(Kind::ContainerStart, "Start", &["start", n]));
    }
    out.push(Action::rekcod(Kind::ContainerLogs, "Logs", &["logs", "-f", "--tail", "200", n]).interactive());
    out.push(Action::rekcod(Kind::ContainerRemove, "Remove", &["rm", "-f", n]).confirm(format!("Remove container {n}?")));
    out
}

pub fn port_actions(p: &Port) -> Vec<Action> {
    if p.error.is_some() || p.udp {
        return Vec::new();
    }
    vec![Action {
        kind: Kind::OpenPort,
        label: format!("Open http://localhost:{}", p.port),
        run: Run::Open(format!("http://localhost:{}", p.port)),
        dir: None,
        interactive: false,
        confirm: None,
    }]
}

pub fn project_actions(p: &Project) -> Vec<Action> {
    let mut save = Action::rekcod(Kind::SnapSave, "Save snapshot (git branch or timestamp)", &["snap", "save", "-p", &p.name]);
    save.dir = p.dir.clone().filter(|d| d.is_dir());
    vec![save]
}

pub fn snapshot_actions(s: &Snapshot) -> Vec<Action> {
    let (p, n) = (s.project.as_str(), s.name.as_str());
    vec![
        Action::rekcod(Kind::SnapRestore, "Restore", &["snap", "restore", "-p", p, n])
            .confirm(format!("Restore {p} to snapshot {n}? Its containers restart and current volume data is replaced.")),
        Action::rekcod(Kind::SnapRemove, "Delete", &["snap", "rm", "-p", p, n]).confirm(format!("Delete snapshot {n} of {p}?")),
    ]
}

pub fn age(created: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    match now.saturating_sub(created) {
        a if a < 120 => format!("{a}s ago"),
        a if a < 7200 => format!("{}m ago", a / 60),
        a if a < 172_800 => format!("{}h ago", a / 3600),
        a => format!("{}d ago", a / 86_400),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn sample() -> (Vec<Container>, Vec<Port>, Vec<Project>) {
        let containers = parse_containers(
            br#"[{"Id":"a","Names":["/web"],"Image":"nginx","State":"running","Status":"Up 2 minutes",
                  "Ports":[{"PublicPort":8080},{"PublicPort":8080}],"Labels":{"com.docker.compose.project":"shop"}},
                 {"Id":"b","Names":["/old"],"Image":"alpine","State":"exited","Status":"Exited (0)","Labels":{}}]"#,
        );
        let ports = ports_of(
            &serde_json::json!({"ports": [
                {"proto":"tcp","address":"127.0.0.1","port":3000,"container":"x"},
                {"proto":"tcp","address":"::1","port":3000,"container":"x"},
                {"proto":"udp","address":"::","port":53,"container":"dns"}]}),
            false,
        );
        let snaps = parse_snapshots(
            br#"{"Volumes":[
                {"Name":"rekcod-snap.shop.seeded.shop_pg","Labels":{"dev.rekcod.snap.project":"shop","dev.rekcod.snap.name":"seeded","dev.rekcod.snap.volume":"shop_pg","dev.rekcod.snap.created":"10"}},
                {"Name":"rekcod-snap.shop.seeded.shop_cache","Labels":{"dev.rekcod.snap.project":"shop","dev.rekcod.snap.name":"seeded","dev.rekcod.snap.volume":"shop_cache","dev.rekcod.snap.created":"12"}},
                {"Name":"plain","Labels":null}]}"#,
        );
        let projects = projects(&containers, b"[]", snaps);
        (containers, ports, projects)
    }

    #[test]
    fn model_parsing() {
        let (containers, ports, projects) = sample();
        assert_eq!(containers.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["web", "old"]);
        assert_eq!(containers[0].ports, vec![8080]);
        assert_eq!(ports.len(), 2, "127.0.0.1 + ::1 is one port");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].snapshots[0].volumes, vec!["shop_cache", "shop_pg"]);
        assert_eq!(projects[0].snapshots[0].created, 12);
    }

    /// Every action kind comes out of the lists the menu bar renders.
    pub fn reachable_kinds() -> Vec<Action> {
        let (containers, ports, projects) = sample();
        let mut all = vm_actions(true, false);
        all.extend(vm_actions(false, true));
        for c in &containers {
            all.extend(container_actions(c));
        }
        for p in &ports {
            all.extend(port_actions(p));
        }
        for p in &projects {
            all.extend(project_actions(p));
            for s in &p.snapshots {
                all.extend(snapshot_actions(s));
            }
        }
        all
    }

    #[test]
    fn every_kind_is_offered() {
        let actions = reachable_kinds();
        for kind in Kind::ALL {
            assert!(actions.iter().any(|a| a.kind == kind), "{kind:?} is not offered by any *_actions list");
        }
    }


    #[test]
    fn destructive_actions_ask_first() {
        for a in reachable_kinds() {
            let destructive = matches!(a.kind, Kind::VmStop | Kind::ContainerRemove | Kind::SnapRestore | Kind::SnapRemove);
            assert_eq!(a.confirm.is_some(), destructive, "{:?}", a.kind);
        }
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
