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
    SandboxShell,
    SandboxDiff,
    SandboxApply,
    SandboxStop,
    SandboxRemove,
    SandboxScreenshot,
    AppOpen,
    AppLogs,
    AppRollback,
    AppDestroy,
    AppPublic,
    AppPrivate,
    AppWake,
    AppRecycle,
    JobLogs,
    JobCancel,
    NsUse,
    HostUse,
    HostForward,
    EdgeBan,
    EdgeUnban,
    StorageUp,
    StorageEnv,
    StorageDown,
    ProjectRestart,
    ProjectStop,
    ProjectDown,
    ImageRemove,
    VolumeRemove,
    ImagePrune,
    VolumePrune,
    BuildCachePrune,
    JobRerun,
    NodeDrain,
    NodeUndrain,
    BackupNow,
    RemoteUse,
    DashboardOpen,
    VmRepair,
    VmForceStop,
    VmAutostartOn,
    VmAutostartOff,
}

impl Kind {
    /// Every kind; the parity tests check each one reaches the menu bar.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: [Kind; 56] = [
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
        Kind::SandboxShell,
        Kind::SandboxDiff,
        Kind::SandboxApply,
        Kind::SandboxStop,
        Kind::SandboxRemove,
        Kind::SandboxScreenshot,
        Kind::AppOpen,
        Kind::AppLogs,
        Kind::AppRollback,
        Kind::AppDestroy,
        Kind::AppPublic,
        Kind::AppPrivate,
        Kind::AppWake,
        Kind::AppRecycle,
        Kind::JobLogs,
        Kind::JobCancel,
        Kind::NsUse,
        Kind::HostUse,
        Kind::HostForward,
        Kind::EdgeBan,
        Kind::EdgeUnban,
        Kind::StorageUp,
        Kind::StorageEnv,
        Kind::StorageDown,
        Kind::ProjectRestart,
        Kind::ProjectStop,
        Kind::ProjectDown,
        Kind::ImageRemove,
        Kind::VolumeRemove,
        Kind::ImagePrune,
        Kind::VolumePrune,
        Kind::BuildCachePrune,
        Kind::JobRerun,
        Kind::NodeDrain,
        Kind::NodeUndrain,
        Kind::BackupNow,
        Kind::RemoteUse,
        Kind::DashboardOpen,
        Kind::VmRepair,
        Kind::VmForceStop,
        Kind::VmAutostartOn,
        Kind::VmAutostartOff,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub project: Option<String>,
    /// `com.docker.compose.service`.
    pub service: Option<String>,
    pub ports: Vec<u16>,
    pub image_id: String,
    /// Named volumes it mounts.
    pub volumes: Vec<String>,
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
    pub containers: usize,
    pub running: usize,
    /// The git branch checked out in `dir`: the snapshot name `snap restore` picks.
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Image {
    pub id: String,
    pub tags: Vec<String>,
    pub size: u64,
    pub created: u64,
    /// Containers (any state) created from it.
    pub containers: Vec<String>,
    /// Untagged and not a parent: `image prune` removes it.
    pub dangling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    /// From the last `/system/df` (cached, see `disk_usage`).
    pub size: Option<u64>,
    pub containers: Vec<String>,
    /// No name given (`com.docker.volume.anonymous`): what `volume prune` removes when unused.
    pub anonymous: bool,
    /// A `rekcod snap save` copy; never pruned.
    pub snapshot: bool,
}

/// `/system/df` totals, as of `checked`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Disk {
    pub checked: u64,
    pub images: u64,
    pub containers: u64,
    pub volumes: u64,
    pub build_cache: u64,
    /// Build cache not in use: what `builder prune` frees.
    pub build_cache_free: u64,
}

/// A fleet node, as the admin-only `GET /nodes` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FleetNode {
    pub id: String,
    pub state: String,
    pub last_seen: u64,
}

/// A client IP the edges saw in the last 10 minutes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EdgeClient {
    pub ip: String,
    pub requests: u64,
    pub limited: u64,
}

/// Configured fleet remotes (`rekcod remote ls`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteList {
    pub default: Option<String>,
    pub all: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub project: String,
    pub name: String,
    pub created: u64,
    /// The project volumes it holds, sorted.
    pub volumes: Vec<String>,
    /// The snapshot volumes holding them, in the same order.
    pub volume_names: Vec<String>,
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
    /// The VM starts at login (`rekcod start --install`).
    pub autostart: bool,
    /// Agent sandboxes (`rekcod sandbox ls`).
    pub sandboxes: Vec<AgentSandbox>,
    /// Apps on the default fleet remote (`rekcod app ls`), if one is configured.
    pub apps: Vec<RemoteApp>,
    /// Recent jobs on the default fleet remote (`rekcod job ls`).
    pub jobs: Vec<RemoteJob>,
    /// The default remote's namespaces (`rekcod ns ls`); apps and jobs above are the current one's.
    pub namespaces: Namespaces,
    /// Remote docker hosts (`rekcod host ls`).
    pub hosts: Hosts,
    /// The edge's banned addresses (`rekcod edge bans`); `None` unless the default remote
    /// has an admin token.
    pub bans: Option<Vec<String>>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub disk: Option<Disk>,
    /// Fleet nodes; `None` without an admin token.
    pub nodes: Option<Vec<FleetNode>>,
    /// The busiest client IPs on the edges (admin token).
    pub edge_clients: Vec<EdgeClient>,
    pub remotes: RemoteList,
    /// The default remote's token is an admin token.
    pub admin: bool,
    /// What each controller call that failed said, by what it was for (`apps`, `jobs`, ...).
    pub remote_errors: std::collections::BTreeMap<String, String>,
}

/// Docker contexts `rekcod-*`, and whether the local `rekcod` context is in use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hosts {
    pub all: Vec<crate::host::Host>,
    pub local_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteApp {
    pub name: String,
    pub url: String,
    pub live_release: Option<i64>,
    pub public: bool,
    /// Scaled to zero; the next request (or `app wake`) starts it.
    pub sleeping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteJob {
    pub id: String,
    pub name: Option<String>,
    pub state: String,
    pub exit: Option<i64>,
    pub created: u64,
}

/// Namespaces on the default remote: the current one and every one this token is in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Namespaces {
    pub current: String,
    pub all: Vec<String>,
    /// `GET /namespaces` as is: role, quota, usage and members of each.
    pub detail: Vec<Value>,
}

/// What `Model::load` reads from the default remote. Each failed call leaves its error in
/// `errors`; "admin token required" isn't one, it means the list isn't offered.
#[derive(Default)]
struct RemoteData {
    apps: Vec<RemoteApp>,
    jobs: Vec<RemoteJob>,
    namespaces: Namespaces,
    bans: Option<Vec<String>>,
    nodes: Option<Vec<FleetNode>>,
    clients: Vec<EdgeClient>,
    admin: bool,
    errors: std::collections::BTreeMap<String, String>,
}

fn remote_data(paths: &Paths) -> RemoteData {
    let mut d = RemoteData::default();
    let Ok(r) = crate::fleet::remote(paths, None) else { return d };
    let get = |path: &str| crate::fleet::call(&r.url, "GET", path, Some(&r.token), &Value::Null);
    let err = |errors: &mut std::collections::BTreeMap<String, String>, what: &str, e: anyhow::Error| {
        errors.insert(what.into(), format!("{e:#}"));
    };
    match get(&r.scoped("/apps")) {
        Ok(v) => d.apps = parse_apps(&v),
        Err(e) => err(&mut d.errors, "apps", e),
    }
    // A controller that didn't answer won't answer the rest either.
    if d.errors.get("apps").is_some_and(|e| unreachable(e)) {
        d.namespaces.current = r.ns();
        return d;
    }
    match get(&r.scoped("/jobs")) {
        Ok(v) => d.jobs = parse_jobs(&v, 10),
        Err(e) => err(&mut d.errors, "jobs", e),
    }
    match get("/namespaces") {
        Ok(v) => {
            d.namespaces.all = v.as_array().into_iter().flatten().filter_map(|n| n["name"].as_str().map(String::from)).collect();
            d.namespaces.detail = v.as_array().cloned().unwrap_or_default();
        }
        Err(e) => err(&mut d.errors, "namespaces", e),
    }
    d.namespaces.current = r.ns();
    match get("/nodes") {
        Ok(v) => {
            d.admin = true;
            d.nodes = Some(
                v.as_array()
                    .into_iter()
                    .flatten()
                    .map(|n| FleetNode {
                        id: n["id"].as_str().unwrap_or_default().into(),
                        state: n["state"].as_str().unwrap_or_default().into(),
                        last_seen: n["last_seen"].as_u64().unwrap_or(0),
                    })
                    .collect(),
            )
        }
        Err(e) if !admin_only(&e) => err(&mut d.errors, "nodes", e),
        Err(_) => {}
    }
    if d.admin {
        match get("/bans") {
            Ok(v) => d.bans = Some(v.as_array().into_iter().flatten().filter_map(|b| b["net"].as_str().map(String::from)).collect()),
            Err(e) => err(&mut d.errors, "bans", e),
        }
        // Older controllers don't have it; that isn't worth an error.
        if let Ok(v) = get("/edge/clients") {
            d.clients = v
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| EdgeClient {
                    ip: c["ip"].as_str().unwrap_or_default().into(),
                    requests: c["requests"].as_u64().unwrap_or(0),
                    limited: c["limited"].as_u64().unwrap_or(0),
                })
                .collect();
        }
    }
    d
}

/// The controller refused because the token isn't an admin one.
pub fn admin_only(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("admin token required")
}

/// Connection-level failures (as opposed to the controller answering with an error).
pub fn unreachable(e: &str) -> bool {
    ["connect", "refused", "timed out", "resolve", "unreachable", "reset", "No route"].iter().any(|w| e.contains(w))
}

pub fn parse_jobs(v: &Value, max: usize) -> Vec<RemoteJob> {
    v["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .take(max)
        .map(|j| RemoteJob {
            id: j["id"].as_str().unwrap_or("").into(),
            name: j["name"].as_str().map(String::from),
            state: j["state"].as_str().unwrap_or("").into(),
            exit: j["exit"].as_i64(),
            created: j["created"].as_u64().unwrap_or(0),
        })
        .collect()
}

pub fn parse_apps(v: &Value) -> Vec<RemoteApp> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|a| RemoteApp {
            name: a["name"].as_str().unwrap_or("").into(),
            url: a["url"].as_str().unwrap_or("").into(),
            live_release: a["live_release"].as_i64(),
            public: a["public"].as_bool().unwrap_or(false),
            sleeping: a["sleeping"].as_bool().unwrap_or(false),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentSandbox {
    pub id: String,
    pub name: Option<String>,
    pub project: PathBuf,
    pub copy: bool,
    pub running: bool,
    pub macos: bool,
    pub created: u64,
    pub cpus: usize,
    pub memory_gib: u64,
    pub pid: Option<i32>,
}

impl Model {
    /// Everything, the fleet included.
    pub fn load(paths: &Paths) -> Model {
        Model::load_with(paths, true)
    }

    /// `remote: false` leaves out every controller call (`rekcod state --local`).
    pub fn load_with(paths: &Paths, remote: bool) -> Model {
        let status = crate::cli::status_value(paths);
        let running = status["running"] == true;
        let sandbox = crate::cli::sandbox_status(paths);
        let socket_installed = crate::syssock::installed(paths);
        let sandboxes = crate::sandbox::list(paths)
            .into_iter()
            .map(|l| AgentSandbox {
                id: l.meta.id,
                name: l.meta.name,
                project: l.meta.project,
                copy: l.meta.mode == "copy",
                macos: l.meta.os == "macos",
                running: l.running,
                created: l.meta.created,
                cpus: l.meta.cpus,
                memory_gib: l.meta.memory_gib,
                pid: l.pid,
            })
            .collect();
        let rd = if remote { remote_data(paths) } else { RemoteData::default() };
        let remotes = {
            let r = crate::fleet::remotes(paths);
            RemoteList { default: r.default, all: r.remotes.keys().cloned().collect() }
        };
        let (all, local_current) = crate::host::list();
        let hosts = Hosts { all, local_current };
        let mut ports = ports_of(&status, false);
        if let Some(sb) = &sandbox {
            ports.extend(ports_of(sb, true));
        }
        let mut m = Model {
            running,
            status,
            ports,
            sandbox,
            socket_installed,
            autostart: crate::cli::autostart_installed(),
            sandboxes,
            apps: rd.apps,
            jobs: rd.jobs,
            namespaces: rd.namespaces,
            hosts,
            nodes: rd.nodes,
            edge_clients: rd.clients,
            remotes,
            admin: rd.admin,
            remote_errors: rd.errors,
            bans: rd.bans,
            ..Default::default()
        };
        if !running {
            return m;
        }
        let raw = crate::cli::api_get(paths, "/containers/json?all=1").unwrap_or_default();
        m.containers = parse_containers(&raw);
        let filter = "%7B%22label%22%3A%5B%22dev.rekcod.snap%22%5D%7D"; // {"label":["dev.rekcod.snap"]}
        let snapshots =
            crate::cli::api_get(paths, &format!("/volumes?filters={filter}")).ok().map(|b| parse_snapshots(&b)).unwrap_or_default();
        m.projects = projects(&m.containers, &raw, snapshots);
        let df = disk_usage(paths, remote);
        m.images = crate::cli::api_get(paths, "/images/json").map(|b| parse_images(&b, &m.containers)).unwrap_or_default();
        m.volumes = crate::cli::api_get(paths, "/volumes").map(|b| parse_volumes(&b, &m.containers, df.as_ref())).unwrap_or_default();
        m.disk = df.as_ref().map(parse_disk);
        m
    }
}

/// `/system/df` (it takes seconds: it sizes every volume), cached in `run/df.json` for two
/// minutes. Only `rekcod state` (`refresh`) measures again; the menu bar reads the cache.
fn disk_usage(paths: &Paths, refresh: bool) -> Option<Value> {
    let f = paths.home.join("run").join("df.json");
    let cached: Option<Value> = std::fs::read(&f).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let fresh = cached.as_ref().and_then(|v| v["checked"].as_u64()).is_some_and(|t| now_secs().saturating_sub(t) < 120);
    if fresh || !refresh {
        return cached;
    }
    let mut v: Value = crate::cli::api_get(paths, "/system/df").ok().and_then(|b| serde_json::from_slice(&b).ok())?;
    v["checked"] = now_secs().into();
    let _ = std::fs::create_dir_all(f.parent()?);
    let _ = std::fs::write(&f, v.to_string());
    Some(v)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub fn parse_images(json: &[u8], containers: &[Container]) -> Vec<Image> {
    let list: Vec<Value> = serde_json::from_slice(json).unwrap_or_default();
    let mut out: Vec<Image> = list
        .iter()
        .map(|i| {
            let id = i["Id"].as_str().unwrap_or_default().to_string();
            let tags: Vec<String> = i["RepoTags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str())
                .filter(|t| *t != "<none>:<none>")
                .map(String::from)
                .collect();
            Image {
                containers: containers.iter().filter(|c| c.image_id == id).map(|c| c.name.clone()).collect(),
                dangling: tags.is_empty(),
                size: i["Size"].as_u64().unwrap_or(0),
                created: i["Created"].as_u64().unwrap_or(0),
                tags,
                id,
            }
        })
        .collect();
    out.sort_by(|a, b| (a.tags.is_empty(), a.tags.first(), &a.id).cmp(&(b.tags.is_empty(), b.tags.first(), &b.id)));
    out
}

pub fn parse_volumes(json: &[u8], containers: &[Container], df: Option<&Value>) -> Vec<Volume> {
    let v: Value = serde_json::from_slice(json).unwrap_or_default();
    let sizes: std::collections::HashMap<&str, u64> = df
        .and_then(|d| d["Volumes"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v["Name"].as_str()?, v["UsageData"]["Size"].as_i64().filter(|s| *s >= 0)? as u64)))
        .collect();
    let mut out: Vec<Volume> = v["Volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|vol| {
            let name = vol["Name"].as_str().unwrap_or_default().to_string();
            let labels = &vol["Labels"];
            Volume {
                size: sizes.get(name.as_str()).copied(),
                containers: containers.iter().filter(|c| c.volumes.contains(&name)).map(|c| c.name.clone()).collect(),
                driver: vol["Driver"].as_str().unwrap_or_default().into(),
                anonymous: labels.get("com.docker.volume.anonymous").is_some(),
                snapshot: labels.get("dev.rekcod.snap").is_some(),
                name,
            }
        })
        .collect();
    out.sort_by(|a, b| (a.anonymous, &a.name).cmp(&(b.anonymous, &b.name)));
    out
}

fn parse_disk(df: &Value) -> Disk {
    let sum = |k: &str, f: &dyn Fn(&Value) -> u64| df[k].as_array().into_iter().flatten().map(f).sum::<u64>();
    let n = |v: &Value| v.as_i64().filter(|n| *n > 0).unwrap_or(0) as u64;
    Disk {
        checked: df["checked"].as_u64().unwrap_or(0),
        images: n(&df["LayersSize"]),
        containers: sum("Containers", &|c| n(&c["SizeRw"])),
        volumes: sum("Volumes", &|v| n(&v["UsageData"]["Size"])),
        build_cache: sum("BuildCache", &|b| n(&b["Size"])),
        build_cache_free: sum("BuildCache", &|b| if b["InUse"] == true || b["Shared"] == true { 0 } else { n(&b["Size"]) }),
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
                service: c["Labels"]["com.docker.compose.service"].as_str().map(String::from),
                ports,
                image_id: c["ImageID"].as_str().unwrap_or_default().to_string(),
                volumes: c["Mounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| m["Type"] == "volume")
                    .filter_map(|m| m["Name"].as_str().map(String::from))
                    .collect(),
            }
        })
        .collect();
    // Running first, then by name.
    out.sort_by(|a, b| (a.state != "running", &a.name).cmp(&(b.state != "running", &b.name)));
    out
}

/// Snapshots from `GET /volumes` (labels written by `rekcod snap save`); see `snap::parse`.
pub fn parse_snapshots(json: &[u8]) -> Vec<Snapshot> {
    let mut out: Vec<Snapshot> = crate::snap::parse(json)
        .into_iter()
        .filter(|s| !s.volumes.is_empty())
        .map(|s| Snapshot {
            project: s.project,
            name: s.name,
            created: s.created,
            volumes: s.volumes.keys().cloned().collect(),
            volume_names: s.volumes.values().cloned().collect(),
        })
        .collect();
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
        let mine = containers.iter().filter(|x| x.project.as_ref() == Some(name));
        let (n, up) = mine.fold((0, 0), |(n, up), x| (n + 1, up + usize::from(x.state == "running")));
        let branch = dir.as_deref().and_then(git_branch);
        out.push(Project { name: name.clone(), dir, snapshots: Vec::new(), containers: n, running: up, branch });
    }
    for s in snapshots {
        match out.iter_mut().find(|p| p.name == s.project) {
            Some(p) => p.snapshots.push(s),
            None => out.push(Project { name: s.project.clone(), dir: None, snapshots: vec![s], containers: 0, running: 0, branch: None }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `dir`'s checked-out branch, normalized as `snap` names snapshots.
fn git_branch(dir: &std::path::Path) -> Option<String> {
    let out = Command::new("git").args(["-C", &dir.to_string_lossy(), "symbolic-ref", "--short", "-q", "HEAD"]).output().ok()?;
    let b = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !b.is_empty()).then(|| crate::snap::normalize_name(&b))
}

// -------------------------------------------------------------- actions

pub fn vm_actions(running: bool, socket_installed: bool, autostart: bool) -> Vec<Action> {
    let mut out = if !running {
        vec![
            Action::rekcod(Kind::VmStart, "Start rekcod", &["start"]),
            Action::rekcod(Kind::VmRepair, "Repair the data disk and start", &["start", "--repair"])
                .confirm(
                    "Run e2fsck -fy on the VM's data disk? Use this when rekcod won't start because of disk errors; it can take minutes."
                        .into(),
                )
                .interactive(),
        ]
    } else {
        vec![
            Action::rekcod(Kind::VmStop, "Stop rekcod", &["stop"])
                .confirm("Stop rekcod? Running containers stop (and `--rm` ones are deleted).".into()),
            Action::rekcod(Kind::VmForceStop, "Force stop rekcod", &["stop", "--force"])
                .confirm("Power the VM off now? Containers don't get to shut down, like pulling the plug.".into()),
            Action::rekcod(Kind::Compact, "Compact data disk", &["compact"]),
            Action::rekcod(Kind::Shell, "Root shell in the VM", &["shell"]).interactive(),
        ]
    };
    out.push(if autostart {
        Action::rekcod(Kind::VmAutostartOff, "Don't start rekcod at login", &["start", "--uninstall"])
    } else {
        Action::rekcod(Kind::VmAutostartOn, "Start rekcod at login", &["start", "--install"])
    });
    if !socket_installed {
        // sudo asks for a password, so it runs in Terminal.
        out.push(Action::rekcod(Kind::SocketInstall, "Use rekcod for /var/run/docker.sock", &["socket", "install"]).interactive());
    }
    out.push(Action::rekcod(Kind::DashboardOpen, "Open the dashboard", &["dashboard"]));
    out
}

/// Local S3 (`rekcod storage up`), while the VM runs; `rekcod-storage` is its container.
pub fn storage_actions(running: bool, containers: &[Container]) -> Vec<Action> {
    if !running {
        return vec![];
    }
    if containers.iter().any(|c| c.name == "rekcod-storage" && c.state == "running") {
        vec![
            Action::rekcod(Kind::StorageEnv, "S3 storage: show env", &["storage", "env"]).interactive(),
            Action::rekcod(Kind::StorageDown, "Stop S3 storage (data kept)", &["storage", "down"]),
        ]
    } else {
        vec![Action::rekcod(Kind::StorageUp, "Start S3 storage (RustFS)", &["storage", "up"]).interactive()]
    }
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
    let n = p.name.as_str();
    let mut out = Vec::new();
    if p.running > 0 {
        out.push(Action::rekcod(Kind::ProjectRestart, "Restart project", &["compose", "-p", n, "restart"]));
        out.push(Action::rekcod(Kind::ProjectStop, "Stop project", &["compose", "-p", n, "stop"]));
    }
    let mut save = Action::rekcod(Kind::SnapSave, "Save snapshot (git branch or timestamp)", &["snap", "save", "-p", n]);
    save.dir = p.dir.clone().filter(|d| d.is_dir());
    out.push(save);
    if p.containers > 0 {
        out.push(
            Action::rekcod(Kind::ProjectDown, "Down (remove containers)", &["compose", "-p", n, "down"])
                .confirm(format!("Remove {n}'s {} container(s) and its networks? Volumes are kept.", p.containers)),
        );
    }
    out
}

pub fn image_actions(i: &Image) -> Vec<Action> {
    let name = i.tags.first().cloned().unwrap_or_else(|| short_id(&i.id).to_string());
    let users = if i.containers.is_empty() { String::new() } else { format!(" {} use it and keep it.", i.containers.join(", ")) };
    vec![
        Action::rekcod(Kind::ImageRemove, "Remove", &["image", "rm", &i.id])
            .confirm(format!("Remove image {name} ({})?{users}", fmt_bytes(i.size))),
    ]
}

pub fn volume_actions(v: &Volume) -> Vec<Action> {
    if !v.containers.is_empty() {
        return vec![];
    }
    let size = v.size.map(|s| format!(" ({})", fmt_bytes(s))).unwrap_or_default();
    vec![
        Action::rekcod(Kind::VolumeRemove, "Remove", &["volume", "rm", &v.name])
            .confirm(format!("Delete volume {}{size} and everything in it?", v.name)),
    ]
}

/// Prunes, while the VM runs. Snapshot volumes are left out of `volume prune`.
pub fn disk_actions(running: bool, images: &[Image], volumes: &[Volume], disk: Option<&Disk>) -> Vec<Action> {
    if !running {
        return vec![];
    }
    let dangling: Vec<&Image> = images.iter().filter(|i| i.dangling && i.containers.is_empty()).collect();
    let loose: Vec<&Volume> = volumes.iter().filter(|v| v.anonymous && !v.snapshot && v.containers.is_empty()).collect();
    let loose_size: Option<u64> = loose.iter().map(|v| v.size).sum();
    let free = |n: Option<u64>| n.map(|b| format!(", freeing {}", fmt_bytes(b))).unwrap_or_default();
    vec![
        Action::rekcod(Kind::ImagePrune, "Remove dangling images", &["image", "prune", "-f"]).confirm(format!(
            "Remove {} dangling image(s){}?",
            dangling.len(),
            free(Some(dangling.iter().map(|i| i.size).sum()))
        )),
        Action::rekcod(
            Kind::VolumePrune,
            "Remove unused anonymous volumes",
            &["volume", "prune", "-f", "--filter", "label!=dev.rekcod.snap"],
        )
        .confirm(format!(
            "Delete {} unused anonymous volume(s){}? Named volumes and snapshots are kept.",
            loose.len(),
            free(loose_size)
        )),
        Action::rekcod(Kind::BuildCachePrune, "Clear the build cache", &["builder", "prune", "-f"])
            .confirm(format!("Clear unused build cache{}? The next builds start cold.", free(disk.map(|d| d.build_cache_free)))),
    ]
}

fn short_id(id: &str) -> &str {
    let id = id.trim_start_matches("sha256:");
    &id[..id.len().min(12)]
}

pub fn fmt_bytes(n: u64) -> String {
    let mut v = n as f64;
    let mut i = 0;
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[i]) }
}

pub fn snapshot_actions(s: &Snapshot) -> Vec<Action> {
    let (p, n) = (s.project.as_str(), s.name.as_str());
    vec![
        Action::rekcod(Kind::SnapRestore, "Restore", &["snap", "restore", "-p", p, n])
            .confirm(format!("Restore {p} to snapshot {n}? Its containers restart and current volume data is replaced.")),
        Action::rekcod(Kind::SnapRemove, "Delete", &["snap", "rm", "-p", p, n]).confirm(format!("Delete snapshot {n} of {p}?")),
    ]
}

pub fn sandbox_actions(s: &AgentSandbox) -> Vec<Action> {
    let id = s.id.as_str();
    let mut out = vec![Action::rekcod(Kind::SandboxShell, "Shell", &["sandbox", "shell", id]).interactive()];
    if s.macos && s.running {
        let png = std::env::temp_dir().join(format!("rekcod-{id}.png"));
        out.push(Action::rekcod(Kind::SandboxScreenshot, "Screenshot", &["sandbox", "screenshot", id, &png.to_string_lossy(), "--open"]));
    }
    if s.copy {
        out.push(Action::rekcod(Kind::SandboxDiff, "Diff", &["sandbox", "diff", id]).interactive());
        out.push(Action::rekcod(Kind::SandboxApply, "Apply to project", &["sandbox", "apply", id]).confirm(format!(
            "Write sandbox {id}'s changes into {}? Files the sandbox changed or deleted are overwritten or removed.",
            s.project.display()
        )));
    }
    if s.running {
        out.push(Action::rekcod(Kind::SandboxStop, "Stop", &["sandbox", "stop", id]));
    }
    out.push(
        Action::rekcod(Kind::SandboxRemove, "Delete", &["sandbox", "rm", id])
            .confirm(format!("Delete sandbox {id} and everything in it? Unapplied changes are lost.")),
    );
    out
}

pub fn app_actions(a: &RemoteApp) -> Vec<Action> {
    let n = a.name.as_str();
    vec![
        Action {
            kind: Kind::AppOpen,
            label: format!("Open {}", a.url),
            run: Run::Open(a.url.clone()),
            dir: None,
            interactive: false,
            confirm: None,
        },
        Action::rekcod(Kind::AppLogs, "Logs", &["app", "logs", n]).interactive(),
        if a.sleeping {
            Action::rekcod(Kind::AppWake, "Wake (it's scaled to zero)", &["app", "wake", n])
        } else {
            Action::rekcod(Kind::AppRecycle, "Restart instances (fresh VMs)", &["app", "recycle", n])
        },
        Action::rekcod(Kind::AppRollback, "Roll back to the previous release", &["app", "rollback", n])
            .confirm(format!("Roll {n} back to its previous good release?")),
        if a.public {
            Action::rekcod(Kind::AppPrivate, "Make private", &["app", "private", n])
        } else {
            Action::rekcod(Kind::AppPublic, "Make public", &["app", "public", n])
                .confirm(format!("Serve {n} on the edge's public listener? Anyone with the URL can reach it."))
        },
        Action::rekcod(Kind::AppDestroy, "Destroy", &["app", "destroy", n])
            .confirm(format!("Destroy {n}? Its instances and release history are deleted.")),
    ]
}

pub fn job_actions(j: &RemoteJob) -> Vec<Action> {
    let id = j.id.as_str();
    if j.state == "running" {
        vec![
            Action::rekcod(Kind::JobLogs, "Output", &["job", "logs", id, "--follow"]).interactive(),
            Action::rekcod(Kind::JobCancel, "Cancel", &["job", "cancel", id]).confirm(format!("Cancel job {id}? Its sandbox is deleted.")),
        ]
    } else {
        vec![
            Action::rekcod(Kind::JobLogs, "Output", &["job", "logs", id]).interactive(),
            Action::rekcod(Kind::JobRerun, "Run again", &["job", "rerun", id]),
        ]
    }
}

/// Admin token only: drain a node, or put a drained one back.
pub fn node_actions(n: &FleetNode) -> Vec<Action> {
    let id = n.id.as_str();
    if n.state == "drain" {
        vec![Action::rekcod(Kind::NodeUndrain, "Undrain (place sandboxes again)", &["node", "undrain", id])]
    } else {
        vec![
            Action::rekcod(Kind::NodeDrain, "Drain", &["node", "drain", id])
                .confirm(format!("Drain {id}? Nothing new is placed there; what runs keeps running.")),
        ]
    }
}

/// The controller itself, with an admin token.
pub fn controller_actions(admin: bool) -> Vec<Action> {
    if !admin {
        return vec![];
    }
    vec![Action::rekcod(Kind::BackupNow, "Back up the controller now", &["controller", "backup"])]
}

/// Make another configured remote the default.
pub fn remote_actions(r: &RemoteList) -> Vec<Action> {
    r.all
        .iter()
        .filter(|n| Some(*n) != r.default.as_ref())
        .map(|n| Action::rekcod(Kind::RemoteUse, format!("Use remote {n}"), &["remote", "use", n]))
        .collect()
}

/// Switch the default remote to another namespace (the current one isn't offered).
pub fn namespace_actions(n: &Namespaces) -> Vec<Action> {
    n.all
        .iter()
        .filter(|ns| **ns != n.current)
        .map(|ns| Action::rekcod(Kind::NsUse, format!("Switch to {ns}"), &["ns", "use", ns]))
        .collect()
}

/// Switch the docker context between this Mac's rekcod and remote hosts; forward a host's ports.
pub fn host_actions(h: &Hosts) -> Vec<Action> {
    let mut out = Vec::new();
    if !h.local_current {
        out.push(Action::rekcod(Kind::HostUse, "Use local rekcod", &["host", "use", "local"]));
    }
    for host in &h.all {
        if !host.current {
            out.push(Action::rekcod(Kind::HostUse, format!("Use {}", host.name), &["host", "use", &host.name]));
        }
    }
    for host in &h.all {
        out.push(
            Action::rekcod(Kind::HostForward, format!("Forward ports from {}", host.name), &["host", "forward", &host.name]).interactive(),
        );
    }
    out
}

/// Ban an address on every edge (one typed in, or one of the busiest clients), or lift a ban.
pub fn ban_actions(nets: &[String], clients: &[EdgeClient]) -> Vec<Action> {
    let mut out = vec![Action::rekcod(Kind::EdgeBan, "Ban an IP or range", &["edge", "ban"]).interactive()];
    for c in clients.iter().filter(|c| !nets.iter().any(|n| n == &c.ip || n.strip_suffix("/32") == Some(&c.ip))) {
        out.push(Action::rekcod(Kind::EdgeBan, format!("Ban {} ({} requests)", c.ip, c.requests), &["edge", "ban", &c.ip]).confirm(
            format!("Ban {} on every edge? It made {} requests ({} rate-limited) in the last 10 minutes.", c.ip, c.requests, c.limited),
        ));
    }
    out.extend(nets.iter().map(|n| Action::rekcod(Kind::EdgeUnban, format!("Unban {n}"), &["edge", "unban", n])));
    out
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

    /// `rekcod storage up`'s container, running.
    pub fn sample_storage() -> Container {
        Container { id: "c".into(), name: "rekcod-storage".into(), state: "running".into(), ..Default::default() }
    }

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

    pub fn sample_app() -> RemoteApp {
        RemoteApp { name: "dash".into(), url: "http://dash.apps.test".into(), live_release: Some(3), public: false, sleeping: false }
    }

    pub fn sample_job() -> RemoteJob {
        RemoteJob { id: "j-abcdefgh".into(), name: Some("etl".into()), state: "running".into(), exit: None, created: 0 }
    }

    pub fn sample_namespaces() -> Namespaces {
        Namespaces { current: "default".into(), all: vec!["default".into(), "team".into()], detail: vec![] }
    }

    pub fn sample_hosts() -> Hosts {
        let h = |n: &str, current| crate::host::Host { name: n.into(), endpoint: format!("ssh://me@{n}"), current };
        Hosts { all: vec![h("spare", true), h("mini", false)], local_current: false }
    }

    pub fn sample_bans() -> Vec<String> {
        vec!["203.0.113.0/24".into()]
    }

    pub fn sample_sandbox() -> AgentSandbox {
        AgentSandbox {
            id: "abcdefgh".into(),
            name: Some("try1".into()),
            project: "/Users/me/app".into(),
            copy: true,
            running: true,
            macos: true,
            ..Default::default()
        }
    }

    pub fn sample_images() -> Vec<Image> {
        vec![
            Image {
                id: "sha256:aaa".into(),
                tags: vec!["nginx:latest".into()],
                size: 1 << 20,
                containers: vec!["web".into()],
                ..Default::default()
            },
            Image { id: "sha256:bbb".into(), size: 2 << 20, dangling: true, ..Default::default() },
        ]
    }

    pub fn sample_volumes() -> Vec<Volume> {
        vec![
            Volume { name: "shop_pg".into(), size: Some(5 << 20), containers: vec!["web".into()], ..Default::default() },
            Volume { name: "c9f7af08".into(), size: Some(3 << 20), anonymous: true, ..Default::default() },
            Volume { name: "rekcod-snap.shop.seeded.shop_pg".into(), size: Some(1 << 20), snapshot: true, ..Default::default() },
        ]
    }

    pub fn sample_nodes() -> Vec<FleetNode> {
        vec![
            FleetNode { id: "n-1".into(), state: "ready".into(), last_seen: 0 },
            FleetNode { id: "n-2".into(), state: "drain".into(), last_seen: 0 },
        ]
    }

    pub fn sample_clients() -> Vec<EdgeClient> {
        vec![EdgeClient { ip: "198.51.100.7".into(), requests: 900, limited: 120 }]
    }

    pub fn sample_remotes() -> RemoteList {
        RemoteList { default: Some("home".into()), all: vec!["home".into(), "work".into()] }
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
        let mut all = vm_actions(true, false, false);
        all.extend(vm_actions(false, true, true));
        all.extend(storage_actions(true, &[]));
        all.extend(storage_actions(true, &[sample_storage()]));
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
        all.extend(sandbox_actions(&sample_sandbox()));
        all.extend(app_actions(&sample_app()));
        all.extend(app_actions(&RemoteApp { public: true, sleeping: true, ..sample_app() }));
        all.extend(job_actions(&sample_job()));
        all.extend(job_actions(&RemoteJob { state: "done".into(), exit: Some(0), ..sample_job() }));
        all.extend(namespace_actions(&sample_namespaces()));
        all.extend(host_actions(&sample_hosts()));
        all.extend(ban_actions(&sample_bans(), &sample_clients()));
        for i in &sample_images() {
            all.extend(image_actions(i));
        }
        for v in &sample_volumes() {
            all.extend(volume_actions(v));
        }
        all.extend(disk_actions(
            true,
            &sample_images(),
            &sample_volumes(),
            Some(&Disk { build_cache_free: 7 << 20, ..Default::default() }),
        ));
        for n in &sample_nodes() {
            all.extend(node_actions(n));
        }
        all.extend(controller_actions(true));
        all.extend(remote_actions(&sample_remotes()));
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
            let destructive = matches!(
                a.kind,
                Kind::VmStop
                    | Kind::ContainerRemove
                    | Kind::SnapRestore
                    | Kind::SnapRemove
                    | Kind::SandboxApply
                    | Kind::SandboxRemove
                    | Kind::AppRollback
                    | Kind::AppDestroy
                    | Kind::AppPublic
                    | Kind::JobCancel
                    | Kind::ProjectDown
                    | Kind::ImageRemove
                    | Kind::VolumeRemove
                    | Kind::ImagePrune
                    | Kind::VolumePrune
                    | Kind::BuildCachePrune
                    | Kind::NodeDrain
                    | Kind::VmForceStop
                    | Kind::VmRepair
            ) || (a.kind == Kind::EdgeBan && !a.interactive);
            assert_eq!(a.confirm.is_some(), destructive, "{:?}", a.kind);
        }
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn prunes_say_what_they_free_and_spare_snapshots() {
        let d = disk_actions(true, &sample_images(), &sample_volumes(), Some(&Disk { build_cache_free: 7 << 20, ..Default::default() }));
        let q = |k: Kind| d.iter().find(|a| a.kind == k).and_then(|a| a.confirm.clone()).unwrap();
        assert_eq!(q(Kind::ImagePrune), "Remove 1 dangling image(s), freeing 2.0 MiB?");
        assert!(q(Kind::VolumePrune).starts_with("Delete 1 unused anonymous volume(s), freeing 3.0 MiB?"), "{}", q(Kind::VolumePrune));
        assert!(q(Kind::BuildCachePrune).contains("7.0 MiB"));
        let vp = d.iter().find(|a| a.kind == Kind::VolumePrune).unwrap();
        assert!(matches!(&vp.run, Run::Rekcod(v) if v.contains(&"label!=dev.rekcod.snap".to_string())));
        assert!(volume_actions(&sample_volumes()[0]).is_empty(), "a volume in use can't be removed");
        assert!(disk_actions(false, &[], &[], None).is_empty());
    }

    #[test]
    fn running_jobs_follow_and_finished_ones_rerun() {
        let run = job_actions(&sample_job());
        assert!(matches!(&run[0].run, Run::Rekcod(v) if v.last().map(String::as_str) == Some("--follow")));
        let done = job_actions(&RemoteJob { state: "failed".into(), ..sample_job() });
        assert!(done.iter().any(|a| a.kind == Kind::JobRerun) && !done.iter().any(|a| a.kind == Kind::JobCancel));
        let bans = ban_actions(&["198.51.100.7".into()], &sample_clients());
        assert!(!bans.iter().any(|a| a.kind == Kind::EdgeBan && !a.interactive), "an IP that's banned isn't offered again");
    }
}
