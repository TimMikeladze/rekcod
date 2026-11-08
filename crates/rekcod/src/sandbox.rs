//! Agent sandboxes (docs/agent-sandboxes.md): many throwaway VMs, each a full dockerd
//! that sees one copy of one project. State lives in `~/.rekcod/sb/<id>`, kept short
//! because the docker socket path has to fit in `sun_path`.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::safefs::{Dir, State};

/// The long-running container `exec` runs in.
pub const WS: &str = "rekcod-ws";
const DISK_GIB: u64 = 64;
const DEFAULT_IMAGE: &str = "debian:bookworm-slim";
/// What `golden build` pulls when given no images.
pub const GOLDEN_IMAGES: &[&str] = &["debian:bookworm-slim", "alpine:3.22", "node:lts-slim", "python:3.13-slim"];
/// Not compared by `diff` or written by `apply`: dirs that stay in the VM. Git's own state
/// is left out too, under any spelling ([`is_git`]).
const SKIP: &[&str] = &["node_modules", ".venv", "target"];

/// Whether the Mac's filesystem may take `name` for `.git`: compared without case, and
/// without the code points HFS+ ignores in names (git's own `is_hfs_dotgit` list). `.git`
/// is ASCII, so no other Unicode normalization of it exists; `safefs` also refuses writes
/// that reach the project's real `.git` by inode, whatever the name.
pub fn is_git(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else { return false };
    let folded: String = name
        .chars()
        .filter(|c| !matches!(c, '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'))
        .map(|c| match c {
            '\u{130}' | '\u{131}' => 'i',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    folded == ".git"
}

fn skipped(name: &std::ffi::OsStr) -> bool {
    is_git(name) || SKIP.iter().any(|s| name.as_bytes() == s.as_bytes())
}

/// Egress preset `dev`: package registries, image registries, source hosts, distro mirrors.
pub const DEV_HOSTS: &[&str] = &[
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "*.crates.io",
    "proxy.golang.org",
    "sum.golang.org",
    "rubygems.org",
    "*.rubygems.org",
    "*.docker.io",
    "*.docker.com",
    "ghcr.io",
    "*.githubusercontent.com",
    "github.com",
    "*.github.com",
    "quay.io",
    "*.quay.io",
    "deb.debian.org",
    "security.debian.org",
    "*.ubuntu.com",
    "dl-cdn.alpinelinux.org",
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Meta {
    pub id: String,
    pub name: Option<String>,
    pub project: PathBuf,
    /// "copy" or "mount".
    pub mode: String,
    pub image: String,
    pub golden: Option<String>,
    /// None: internet open. Empty with `offline`: no network.
    pub allow: Option<Vec<String>>,
    pub offline: bool,
    pub cpus: usize,
    pub memory_gib: u64,
    pub ttl_secs: u64,
    pub secrets: Vec<String>,
    /// Unix seconds; `apply` refuses files the project changed after this.
    pub created: u64,
    /// "macos" for a macOS guest (`macos.rs`); empty for Linux.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub os: String,
    /// `IP:PORT`s on the Mac or LAN it may reach (an app instance's RustFS).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reach: Vec<String>,
}

pub struct CreateOptions {
    pub dir: Option<PathBuf>,
    pub name: Option<String>,
    pub mount: bool,
    pub image: Option<String>,
    pub golden: String,
    pub cpus: usize,
    pub memory_gib: u64,
    pub allow_hosts: Vec<String>,
    pub offline: bool,
    pub secrets: Vec<String>,
    pub ttl_secs: u64,
    /// "macos" for a macOS guest; `image` then names the base image.
    pub os: Option<String>,
    pub reach: Vec<String>,
}

impl Default for CreateOptions {
    fn default() -> Self {
        CreateOptions {
            dir: None,
            name: None,
            mount: false,
            image: None,
            golden: "default".into(),
            cpus: 2,
            memory_gib: 2,
            allow_hosts: vec![],
            offline: false,
            secrets: vec![],
            ttl_secs: 30 * 60,
            os: None,
            reach: vec![],
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn root(paths: &Paths) -> PathBuf {
    paths.home.join("sb")
}

fn golden_home(paths: &Paths, name: &str) -> PathBuf {
    paths.home.join("golden").join(name)
}

/// Golden disk and macOS image names become directory names: `[a-z0-9][a-z0-9._-]*`.
pub fn check_name(what: &str, name: &str) -> Result<()> {
    let ok = name.len() <= 64
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && !name.contains("..");
    if !ok {
        bail!("bad {what} name {name:?}: use lowercase letters, digits, '.', '_' and '-'");
    }
    Ok(())
}

/// 8 lowercase base32 characters.
pub fn new_id() -> Result<String> {
    let mut b = [0u8; 8];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b))?;
    Ok(b.iter().map(|x| b"abcdefghijklmnopqrstuvwxyz234567"[(*x & 31) as usize] as char).collect())
}

#[cfg(test)]
use rekcod_proto::host_allowed;

/// The allowlist a sandbox boots with: None opens the internet.
pub fn allowlist(extra: &[String]) -> Option<Vec<String>> {
    if extra.iter().any(|h| h == "*") {
        return None;
    }
    let mut out: Vec<String> = DEV_HOSTS.iter().map(|s| s.to_string()).collect();
    out.extend(extra.iter().cloned());
    Some(out)
}

pub fn clonefile(src: &Path, dst: &Path) -> Result<()> {
    let (s, d) = (CString::new(src.as_os_str().as_bytes())?, CString::new(dst.as_os_str().as_bytes())?);
    if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("clone {} to {}", src.display(), dst.display()));
    }
    Ok(())
}

struct Sandbox {
    paths: Paths,
    meta: Meta,
}

impl Sandbox {
    fn meta_file(home: &Path) -> PathBuf {
        home.join("meta.json")
    }
    fn ws(&self) -> PathBuf {
        self.paths.home.join("ws")
    }
    fn save(&self) -> Result<()> {
        std::fs::write(Self::meta_file(&self.paths.home), serde_json::to_vec_pretty(&self.meta)?)?;
        Ok(())
    }
    fn running(&self) -> bool {
        crate::cli::running_pid(&self.paths).is_some()
    }
    fn manifest(&self) -> Option<Manifest> {
        serde_json::from_slice(&std::fs::read(self.paths.home.join("manifest.json")).ok()?).ok()
    }
    fn save_manifest(&self, m: &Manifest) -> Result<()> {
        std::fs::write(self.paths.home.join("manifest.json"), serde_json::to_vec(m)?)?;
        Ok(())
    }
    fn docker(&self) -> Result<Command> {
        let mut c = crate::docker::any_cli(&self.paths)?;
        c.env("DOCKER_HOST", self.paths.docker_host());
        Ok(c)
    }
    fn macos(&self) -> bool {
        self.meta.os == "macos"
    }
    /// The macOS guest's address, booting it if needed.
    fn guest(&self, main: &Paths) -> Result<String> {
        self.touch();
        crate::macos::boot(main, &self.paths.home, self.meta.ttl_secs)
    }
    fn touch(&self) {
        let _ = std::fs::write(self.paths.home.join("run/last-used"), b"");
    }

    fn boot(&self, main: &Paths) -> Result<()> {
        if self.macos() {
            return self.guest(main).map(|_| ());
        }
        if self.running() {
            return Ok(());
        }
        let m = &self.meta;
        check_capacity(main, m.memory_gib)?;
        self.touch();
        let allow = match (&m.allow, m.offline) {
            (_, true) => Some(String::new()),
            (Some(a), _) => Some(a.join(",")),
            (None, _) => None,
        };
        let opts = crate::cli::BootOptions {
            cpus: Some(m.cpus),
            memory_gib: Some(m.memory_gib),
            base_memory_gib: None,
            disk_gib: DISK_GIB,
            project: Some(m.project.clone()),
            guest_dir: Some(main.guest_dir()?),
            workspace: (m.mode == "copy").then(|| self.ws()),
            allow,
            reach: m.reach.clone(),
            idle_ttl: (m.ttl_secs > 0).then_some(m.ttl_secs),
            repair: false,
        };
        crate::cli::boot(&self.paths, &opts)?;
        // A restarted or forked VM: the workspace container comes back by its restart policy,
        // but a fork's copy may have been cloned while paused.
        let _ = self.docker()?.args(["unpause", WS]).stderr(Stdio::null()).stdout(Stdio::null()).status();
        let _ = self.docker()?.args(["start", WS]).stderr(Stdio::null()).stdout(Stdio::null()).status();
        Ok(())
    }

    fn start_workspace(&self) -> Result<()> {
        let m = &self.meta;
        let p = m.project.to_string_lossy().to_string();
        let mut c = self.docker()?;
        c.args(["run", "-d", "--name", WS, "--restart", "unless-stopped", "--init"]);
        // Raw sockets would skip the guest's egress rules; nothing in a workspace needs them.
        c.args(["--cap-drop", "NET_RAW", "--security-opt", "no-new-privileges"]);
        if m.offline {
            c.args(["--network", "none"]);
        } else {
            c.args(["--network", "host", "--label", &format!("{}=1", crate::ports::AUTOPORTS_LABEL)]);
        }
        c.args(["-v", &format!("{p}:{p}"), "-w", &p]);
        if m.mode == "mount" && m.project.join(".git").is_dir() {
            // A live .git is code the Mac runs: hooks and core.fsmonitor fire on the next
            // `git status`, which shell prompts run on their own.
            c.args(["-v", &format!("{p}/.git:{p}/.git:ro")]);
        }
        if let Some(plan) = crate::xrun::plan(&m.project, &["sh".into()]) {
            for dir in &plan.local_dirs {
                c.args(["-v", &format!("{}:{p}/{dir}", crate::xrun::local_volume(&m.project, dir))]);
            }
        }
        for s in &m.secrets {
            c.args(["-e", s]);
        }
        c.args([&m.image, "sh", "-c", "trap 'exit 0' TERM; while :; do sleep 3600 & wait $!; done"]);
        let out = c.stdin(Stdio::null()).output().context("run docker")?;
        if !out.status.success() {
            bail!("starting the workspace container failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }
}

fn load(home: &Path) -> Option<Meta> {
    serde_json::from_slice(&std::fs::read(Sandbox::meta_file(home)).ok()?).ok()
}

/// Every sandbox, oldest first.
fn all(paths: &Paths) -> Vec<Sandbox> {
    let mut out: Vec<Sandbox> = std::fs::read_dir(root(paths))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some(Sandbox { meta: load(&e.path())?, paths: Paths { home: e.path() } }))
        .collect();
    out.sort_by_key(|s| s.meta.created);
    out
}

/// The sandbox with ID `id`, or else the one named `id`.
fn find(paths: &Paths, id: &str) -> Result<Sandbox> {
    let mut all = all(paths);
    let i = all.iter().position(|s| s.meta.id == id).or_else(|| all.iter().position(|s| s.meta.name.as_deref() == Some(id)));
    i.map(|i| all.swap_remove(i)).ok_or_else(|| anyhow::Error::new(NoSandbox(id.into())))
}

/// A sandbox's state directory and metadata, for the API to authorize before it acts.
pub fn lookup(paths: &Paths, id: &str) -> Result<(PathBuf, Meta)> {
    let s = find(paths, id)?;
    Ok((s.paths.home, s.meta))
}

#[derive(Debug)]
pub struct NoSandbox(pub String);
impl std::fmt::Display for NoSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "no sandbox {}", self.0)
    }
}
impl std::error::Error for NoSandbox {}

/// MiB left for sandboxes before running VMs pass 75% of the Mac's memory; None when the
/// Mac's memory can't be read.
pub fn free_mib(paths: &Paths) -> Option<u64> {
    let host = crate::cli::sysctl_u64("hw.memsize")? >> 20;
    let main = crate::cli::status_value(paths)["memory_mib"].as_u64().unwrap_or(0);
    let sandboxes: u64 = all(paths).iter().filter(|s| s.running()).map(|s| s.meta.memory_gib << 10).sum();
    Some((host * 3 / 4).saturating_sub(main + sandboxes))
}

/// The sandbox's state directory.
pub fn home(paths: &Paths, id: &str) -> Result<PathBuf> {
    Ok(find(paths, id)?.paths.home)
}

/// GiB a sandbox's disk and workspace may need before the Mac's disk is too full to start one.
const MIN_FREE_GIB: u64 = 10;

/// Refuses a sandbox when the Mac's disk is nearly full: a full APFS volume turns into I/O
/// errors in every running VM.
fn check_disk(paths: &Paths) -> Result<()> {
    let free = crate::cli::free_disk_gib(&paths.home).unwrap_or(u64::MAX);
    if free < MIN_FREE_GIB {
        bail!("only {free} GiB free on the Mac's disk; sandboxes need at least {MIN_FREE_GIB}");
    }
    Ok(())
}

/// Refuses a sandbox that would push running VMs past 75% of the Mac's memory. Allowed when
/// the Mac's memory can't be read.
fn check_capacity(paths: &Paths, gib: u64) -> Result<()> {
    let Some(host) = crate::cli::sysctl_u64("hw.memsize").map(|b| b >> 20) else { return Ok(()) };
    let main = crate::cli::status_value(paths)["memory_mib"].as_u64().unwrap_or(0);
    let sandboxes: u64 = all(paths).iter().filter(|s| s.running()).map(|s| s.meta.memory_gib << 10).sum();
    let budget = host * 3 / 4;
    if main + sandboxes + (gib << 10) > budget {
        bail!(
            "not enough memory: the main VM has {main} MiB and running sandboxes {sandboxes} MiB; \
             another {} MiB would pass {budget} MiB (75% of this Mac). `rekcod sandbox rm` one first",
            gib << 10
        );
    }
    Ok(())
}

pub fn create(paths: &Paths, o: CreateOptions) -> Result<Meta> {
    check_name("golden disk", &o.golden)?;
    if let Some(img) = o.image.as_deref().filter(|_| o.os.as_deref() == Some("macos")) {
        check_name("macOS image", img)?;
    }
    let project = o.dir.clone().map_or_else(std::env::current_dir, Ok)?.canonicalize()?;
    if !["/Users", "/Volumes", "/private"].iter().any(|r| project.starts_with(r)) {
        bail!("{} isn't under /Users, /Volumes or /private", project.display());
    }
    if let Some(n) = &o.name
        && find(paths, n).is_ok()
    {
        bail!("a sandbox named {n} already exists");
    }
    for s in &o.secrets {
        if std::env::var_os(s).is_none() {
            bail!("--secret {s}: not set in this shell's environment");
        }
    }
    let macos = o.os.as_deref() == Some("macos");
    if let Some(os) = o.os.as_deref().filter(|os| *os != "macos" && *os != "linux") {
        bail!("--os takes linux or macos, not {os}");
    }
    if macos && (o.offline || !o.allow_hosts.is_empty()) {
        // The macOS guest has plain NAT: no allowlist, and the Mac and LAN are reachable.
        bail!("--offline and --allow-host don't apply to macOS sandboxes; their network isn't filtered");
    }
    check_disk(paths)?;
    if macos {
        let running = macos_running(paths);
        if running >= crate::macos::MAX_RUNNING {
            bail!("{running} macOS sandboxes are running; Apple's licence allows {} per Mac", crate::macos::MAX_RUNNING);
        }
    }
    if macos && !crate::macos::ready(paths, o.image.as_deref().unwrap_or("default")) {
        bail!("macOS image {} isn't ready; see `rekcod macos ls`", o.image.as_deref().unwrap_or("default"));
    }
    let memory_gib = if macos { o.memory_gib.max(8) } else { o.memory_gib };
    check_capacity(paths, memory_gib)?;
    let t0 = Instant::now();
    let id = new_id()?;
    std::fs::create_dir_all(root(paths))?;
    crate::cli::exclude_from_backups(&root(paths));
    let sb = Paths::at(root(paths).join(&id))?;
    let golden = golden_home(paths, &o.golden).join("vm/data.img");
    let golden = if macos {
        if let Err(e) = crate::macos::clone_image(paths, o.image.as_deref().unwrap_or("default"), &sb.home, o.cpus, memory_gib) {
            let _ = std::fs::remove_dir_all(&sb.home);
            return Err(e);
        }
        None
    } else if golden.exists() {
        // APFS clone: instant, copy on write.
        clonefile(&golden, &sb.data_disk())?;
        Some(o.golden.clone())
    } else {
        None
    };
    let image = o
        .image
        .clone()
        .or_else(|| macos.then(|| "default".to_string()))
        .or_else(|| crate::xrun::plan(&project, &["sh".into()]).map(|p| p.image))
        .unwrap_or_else(|| DEFAULT_IMAGE.into());
    let s = Sandbox {
        meta: Meta {
            id: id.clone(),
            name: o.name,
            project: project.clone(),
            mode: if o.mount && !macos { "mount" } else { "copy" }.into(),
            image,
            golden,
            allow: if o.offline { Some(vec![]) } else { allowlist(&o.allow_hosts) },
            offline: o.offline,
            cpus: o.cpus,
            memory_gib,
            ttl_secs: o.ttl_secs,
            secrets: o.secrets,
            created: now(),
            os: if macos { "macos".into() } else { String::new() },
            reach: o.reach,
        },
        paths: sb,
    };
    if !o.mount || macos {
        clonefile(&project, &s.ws())?;
        s.save_manifest(&manifest(&project))?;
    }
    s.save()?;
    let result = if macos {
        s.guest(paths).and_then(|ip| crate::macos::push_ws(paths, &ip, &s.ws()))
    } else {
        s.boot(paths).and_then(|_| s.start_workspace())
    };
    if let Err(e) = result {
        let _ = remove(&s);
        return Err(e);
    }
    eprintln!("rekcod: sandbox {id} ready in {:.1}s", t0.elapsed().as_secs_f64());
    Ok(s.meta)
}

fn log_exec(s: &Sandbox, cmd: &[String], cwd: &str, exit: i32, ms: u128) {
    let line = serde_json::json!({
        "ts": now(), "cmd": cmd, "cwd": cwd, "exit": exit, "ms": ms as u64,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(s.paths.home.join("log.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

pub struct ExecOptions {
    pub workdir: Option<String>,
    pub env: Vec<String>,
    pub timeout_secs: Option<u64>,
    pub tty: bool,
}

/// `docker exec` arguments for a command in the workspace container.
pub fn exec_args(meta: &Meta, o: &ExecOptions, cmd: &[String]) -> Vec<String> {
    let mut a: Vec<String> = vec!["exec".into(), "-i".into()];
    if o.tty {
        a.push("-t".into());
    }
    a.extend(["-w".into(), o.workdir.clone().unwrap_or_else(|| meta.project.to_string_lossy().into())]);
    for e in &o.env {
        a.extend(["-e".into(), e.clone()]);
    }
    a.push(WS.into());
    if let Some(t) = o.timeout_secs {
        // In the container, so a timeout kills the command and not just the client.
        a.extend(["timeout".into(), "-s".into(), "KILL".into(), t.to_string()]);
    }
    a.extend(cmd.iter().cloned());
    a
}

/// Runs `cmd` in the sandbox; returns its exit code.
pub fn exec(paths: &Paths, id: &str, o: ExecOptions, cmd: &[String]) -> Result<i32> {
    if cmd.is_empty() {
        bail!("usage: rekcod sandbox exec ID -- CMD...");
    }
    let s = find(paths, id)?;
    s.boot(paths)?;
    s.touch();
    let t0 = Instant::now();
    let code = if s.macos() {
        let ip = s.guest(paths)?;
        let mut c = crate::macos::exec_command(paths, &ip, o.workdir.as_deref(), &o.env, cmd, o.tty)?;
        run_with_timeout(&mut c, o.timeout_secs)?
    } else {
        let args = exec_args(&s.meta, &o, cmd);
        let status = s.docker()?.args(&args).status().context("run docker")?;
        status.code().unwrap_or(1)
    };
    let cwd = o.workdir.clone().unwrap_or_else(|| s.meta.project.to_string_lossy().into());
    log_exec(&s, cmd, &cwd, code, t0.elapsed().as_millis());
    s.touch();
    Ok(code)
}

/// Runs `c`, killing it after `timeout` seconds (exit 124, like `timeout`).
fn run_with_timeout(c: &mut Command, timeout: Option<u64>) -> Result<i32> {
    let mut child = c.spawn().context("run ssh")?;
    let t0 = Instant::now();
    loop {
        if let Some(st) = child.try_wait()? {
            return Ok(st.code().unwrap_or(1));
        }
        if timeout.is_some_and(|t| t0.elapsed().as_secs() >= t) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(124);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Records an exec the API ran.
pub fn record(paths: &Paths, id: &str, cmd: &[String], exit: i32, ms: u128) {
    if let Ok(s) = find(paths, id) {
        let cwd = s.meta.project.to_string_lossy().to_string();
        log_exec(&s, cmd, &cwd, exit, ms);
        s.touch();
    }
}

pub fn shell(paths: &Paths, id: &str) -> Result<()> {
    let s = find(paths, id)?;
    if s.macos() {
        let ip = s.guest(paths)?;
        let sh = ["/bin/zsh".to_string(), "-l".to_string()];
        return Err(crate::macos::exec_command(paths, &ip, None, &[], &sh, true)?.exec()).context("exec ssh");
    }
    s.boot(paths)?;
    s.touch();
    let tty = unsafe { libc::isatty(0) == 1 };
    let o = ExecOptions { workdir: None, env: vec![], timeout_secs: None, tty };
    let sh = ["sh", "-c", "command -v bash >/dev/null && exec bash || exec sh"].map(String::from);
    Err(s.docker()?.args(exec_args(&s.meta, &o, &sh)).exec()).context("exec docker")
}

/// `ID:/path` becomes the workspace container's path; anything else is a Mac path.
pub fn cp(paths: &Paths, src: &str, dst: &str) -> Result<()> {
    let split = |a: &str| a.split_once(':').filter(|(id, _)| !id.contains('/')).map(|(id, p)| (id.to_string(), p.to_string()));
    let (s, args) = match (split(src), split(dst)) {
        (Some((id, p)), None) => (find(paths, &id)?, [format!("{WS}:{p}"), dst.to_string()]),
        (None, Some((id, p))) => (find(paths, &id)?, [src.to_string(), format!("{WS}:{p}")]),
        (Some(_), Some(_)) => bail!("copy between two sandboxes through the Mac"),
        (None, None) => bail!("one side must be ID:/path"),
    };
    if s.macos() {
        let ip = s.guest(paths)?;
        let remote = |a: &str| a.strip_prefix(&format!("{WS}:")).map(|p| format!("{}@{ip}:{p}", crate::macos::USER));
        let args: Vec<String> = args.iter().map(|a| remote(a).unwrap_or_else(|| a.clone())).collect();
        let key = crate::macos::key(paths)?;
        let ok = Command::new("scp")
            .args(["-q", "-r", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-i"])
            .arg(key)
            .args(&args)
            .status()?
            .success();
        if !ok {
            bail!("scp failed");
        }
        return Ok(());
    }
    s.boot(paths)?;
    s.touch();
    let ok = s.docker()?.arg("cp").args(&args).status()?.success();
    if !ok {
        bail!("docker cp failed");
    }
    Ok(())
}

/// A new sandbox from `id`'s data disk and workspace. Running processes don't carry over.
pub fn fork(paths: &Paths, id: &str, name: Option<String>) -> Result<Meta> {
    let parent = find(paths, id)?;
    if let Some(n) = &name
        && find(paths, n).is_ok()
    {
        bail!("a sandbox named {n} already exists");
    }
    check_capacity(paths, parent.meta.memory_gib)?;
    let t0 = Instant::now();
    let new = new_id()?;
    let child_paths = Paths::at(root(paths).join(&new))?;
    let running = parent.running();
    if parent.macos() {
        let n = macos_running(paths);
        if n >= crate::macos::MAX_RUNNING {
            bail!("{n} macOS sandboxes are running; Apple's licence allows {} per Mac", crate::macos::MAX_RUNNING);
        }
        if running {
            // No pause for macOS guests: flush, then clone what's on disk.
            let ip = parent.guest(paths)?;
            let _ = crate::macos::exec_command(paths, &ip, Some("/"), &[], &["sync".into()], false)?.status();
            let _lock = review_lock(&parent.paths.home)?;
            crate::macos::pull_ws(paths, &ip, &parent.ws())?;
        }
        for f in ["disk.img", "aux.img", "hw.bin", "config.json"] {
            clonefile(&parent.paths.home.join(f), &child_paths.home.join(f))?;
        }
        clonefile(&parent.ws(), &child_paths.home.join("ws"))?;
        crate::macos::reidentify(&child_paths.home)?;
        let child = Sandbox { meta: Meta { id: new.clone(), name, ..parent.meta.clone() }, paths: child_paths };
        if let Err(e) = child.save().and_then(|_| child.boot(paths)) {
            let _ = remove(&child);
            return Err(e);
        }
        eprintln!("rekcod: sandbox {new} forked from {} in {:.1}s", parent.meta.id, t0.elapsed().as_secs_f64());
        return Ok(child.meta);
    }
    if running {
        // Flush, then freeze the workspace's processes while the disk is cloned. dockerd
        // keeps running, so the clone is crash-consistent, like a power cut.
        let _ = parent.docker()?.args(["exec", WS, "sync"]).status();
        let _ = parent.docker()?.args(["pause", WS]).stdout(Stdio::null()).status();
    }
    let cloned = clonefile(&parent.paths.data_disk(), &child_paths.data_disk())
        .and_then(|_| if parent.meta.mode == "copy" { clonefile(&parent.ws(), &child_paths.home.join("ws")) } else { Ok(()) });
    if running {
        let _ = parent.docker()?.args(["unpause", WS]).stdout(Stdio::null()).status();
    }
    let child = Sandbox { meta: Meta { id: new.clone(), name, ..parent.meta.clone() }, paths: child_paths };
    if let Err(e) = cloned.and_then(|_| child.save()).and_then(|_| child.boot(paths)) {
        let _ = remove(&child);
        return Err(e);
    }
    eprintln!("rekcod: sandbox {new} forked from {} in {:.1}s", parent.meta.id, t0.elapsed().as_secs_f64());
    Ok(child.meta)
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct Change {
    /// 'A', 'M' or 'D', from the project's point of view.
    pub kind: char,
    pub path: String,
}

/// Directories (`d`), regular files (`f`) and symlinks (`l`) under `base`, without what
/// [`skipped`] leaves out. FIFOs, sockets and devices are left out too: reading a FIFO the
/// sandbox made would hang `diff`. Symlinked directories are listed, not entered.
fn walk(base: &Path) -> Vec<(PathBuf, char)> {
    fn into(base: &Path, rel: &Path, out: &mut Vec<(PathBuf, char)>) {
        let Ok(rd) = std::fs::read_dir(base.join(rel)) else { return };
        for e in rd.flatten() {
            let name = e.file_name();
            if skipped(&name) {
                continue;
            }
            let r = rel.join(&name);
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    out.push((r.clone(), 'd'));
                    into(base, &r, out);
                }
                Ok(t) if t.is_file() => out.push((r, 'f')),
                Ok(t) if t.is_symlink() => out.push((r, 'l')),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    into(base, Path::new(""), &mut out);
    out
}

/// What [`walk`] finds that `diff` compares: files and symlinks.
fn files(base: &Path) -> std::collections::BTreeSet<PathBuf> {
    walk(base).into_iter().filter(|(_, k)| *k != 'd').map(|(p, _)| p).collect()
}

fn open_nofollow(p: &Path) -> std::io::Result<std::fs::File> {
    std::os::unix::fs::OpenOptionsExt::custom_flags(std::fs::OpenOptions::new().read(true), libc::O_NOFOLLOW | libc::O_NONBLOCK).open(p)
}

/// Compares two files in 64 KiB pieces, not by reading both whole.
fn same_bytes(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(mut fa), Ok(mut fb)) = (open_nofollow(a), open_nofollow(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let Ok(n) = fa.read(&mut ba) else { return false };
        if n == 0 {
            return fb.read(&mut bb[..1]).is_ok_and(|m| m == 0);
        }
        if fb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

fn same(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) else { return false };
    if ma.file_type().is_symlink() || mb.file_type().is_symlink() {
        return std::fs::read_link(a).ok() == std::fs::read_link(b).ok();
    }
    // The bits `apply` writes; it drops setuid, setgid and sticky.
    let mode = |m: &std::fs::Metadata| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777;
    ma.len() == mb.len() && mode(&ma) == mode(&mb) && same_bytes(a, b)
}

/// What `apply` would do to `project` to make it match `ws`.
pub fn changes(project: &Path, ws: &Path) -> Vec<Change> {
    let (a, b) = (files(project), files(ws));
    let mut out: Vec<Change> = Vec::new();
    for p in b.iter() {
        if !a.contains(p) {
            out.push(Change { kind: 'A', path: p.to_string_lossy().into() });
        } else if !same(&project.join(p), &ws.join(p)) {
            out.push(Change { kind: 'M', path: p.to_string_lossy().into() });
        }
    }
    for p in a.difference(&b) {
        out.push(Change { kind: 'D', path: p.to_string_lossy().into() });
    }
    out.sort_by(|x, y| x.path.cmp(&y.path));
    out
}

/// The project as the sandbox copied it: path to (kind, size, mtime ns, mode), directories
/// included. `apply` refuses to touch a path whose state on the Mac has moved on from this.
type Manifest = std::collections::BTreeMap<String, State>;

fn manifest(project: &Path) -> Manifest {
    let Ok(dir) = Dir::open(project) else { return Manifest::new() };
    walk(project).iter().filter_map(|(f, _)| Some((f.to_string_lossy().to_string(), dir.state(f)?))).collect()
}

/// Held across `diff`, `apply` and anything that replaces `ws/`, so what `apply --reviewed`
/// writes is the copy the last `diff` showed, from start to end.
fn review_lock(home: &Path) -> Result<std::fs::File> {
    crate::paths::lock(&home.join("review.lock"))
}

/// A copy-mode sandbox, and its review lock.
fn copy_sandbox(paths: &Paths, id: &str) -> Result<(Sandbox, std::fs::File)> {
    let s = find(paths, id)?;
    if s.meta.mode != "copy" {
        bail!("sandbox {} uses --mount; its edits are already in {}", s.meta.id, s.meta.project.display());
    }
    let lock = review_lock(&s.paths.home)?;
    if s.macos() {
        // The guest's files live on its own disk; bring them back to compare.
        let ip = s.guest(paths)?;
        crate::macos::pull_ws(paths, &ip, &s.ws())?;
    }
    Ok((s, lock))
}

/// A frozen copy of the workspace in `<sandbox>/<name>`, so what's compared and written
/// can't change under us while the sandbox keeps running. Its processes are paused while
/// the directory is cloned, as for `fork`.
/// Callers hold the review lock. The copy is cloned under a temporary name and renamed into
/// place, so it's never seen half made and nothing writes into it once it's there.
fn freeze(s: &Sandbox, name: &str) -> Result<PathBuf> {
    let (dst, tmp) = (s.paths.home.join(name), s.paths.home.join(format!("{name}.new")));
    for d in [&tmp, &dst] {
        if std::fs::symlink_metadata(d).is_ok() {
            std::fs::remove_dir_all(d)?;
        }
    }
    let pause = !s.macos() && s.running();
    if pause {
        let _ = s.docker()?.args(["pause", WS]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let r = clonefile(&s.ws(), &tmp);
    if pause {
        let _ = s.docker()?.args(["unpause", WS]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    r?;
    std::fs::rename(&tmp, &dst)?;
    Ok(dst)
}

/// Bytes a terminal would act on (escape sequences can hide lines from the reviewer),
/// shown as `^[` style text instead. Newlines and tabs stay.
pub fn printable(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\t' => c.to_string(),
            c if (c as u32) < 0x20 => format!("^{}", ((c as u8) + 0x40) as char),
            '\u{7f}' => "^?".into(),
            c if ('\u{80}'..='\u{9f}').contains(&c) => format!("\\u{{{:x}}}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

/// Unified diff of the workspace against the project (`git diff --no-index` per file). The
/// workspace is frozen into `<sandbox>/reviewed`, which `apply --reviewed` writes back.
pub fn patch(paths: &Paths, id: &str) -> Result<(Vec<Change>, String)> {
    let (s, _lock) = copy_sandbox(paths, id)?;
    let ws = freeze(&s, "reviewed")?;
    let ch = changes(&s.meta.project, &ws);
    let mut out = String::new();
    let null = PathBuf::from("/dev/null");
    for c in &ch {
        let (a, b) = (s.meta.project.join(&c.path), ws.join(&c.path));
        let (a, b) = match c.kind {
            'A' => (&null, &b),
            'D' => (&a, &null),
            _ => (&a, &b),
        };
        let o = Command::new("git").args(["diff", "--no-index", "--no-color", "--"]).arg(a).arg(b).output()?;
        // git prints absolute paths minus the leading slash; make them project-relative.
        let text = String::from_utf8_lossy(&o.stdout)
            .replace(&format!("a{}/", s.meta.project.display()), "a/")
            .replace(&format!("b{}/", ws.display()), "b/")
            .replace(&format!("a{}/", ws.display()), "a/")
            .replace(&format!("b{}/", s.meta.project.display()), "b/");
        out.push_str(&text);
    }
    Ok((ch, out))
}

pub fn diff(paths: &Paths, id: &str, stat: bool) -> Result<()> {
    let (ch, p) = patch(paths, id)?;
    if stat {
        for c in &ch {
            println!("{} {}", c.kind, printable(&c.path));
        }
    } else {
        print!("{}", printable(&p));
    }
    if !ch.is_empty() {
        eprintln!(
            "rekcod: `rekcod sandbox apply {id} --reviewed` writes exactly this; without --reviewed it takes the workspace as it is then"
        );
    }
    Ok(())
}

/// A docker command aimed at the sandbox (booted if it was stopped), for the API.
pub fn docker_cmd(paths: &Paths, id: &str) -> Result<(Command, Meta)> {
    let s = find(paths, id)?;
    if s.macos() {
        bail!("sandbox {id} is a macOS guest; it has no docker");
    }
    s.boot(paths)?;
    s.touch();
    Ok((s.docker()?, s.meta))
}

/// The command that runs `cmd` in the sandbox: `docker exec` for Linux, ssh for macOS.
pub fn exec_cmd(paths: &Paths, id: &str, o: &ExecOptions, cmd: &[String]) -> Result<Command> {
    let s = find(paths, id)?;
    if s.macos() {
        let ip = s.guest(paths)?;
        return crate::macos::exec_command(paths, &ip, o.workdir.as_deref(), &o.env, cmd, false);
    }
    s.boot(paths)?;
    s.touch();
    let mut c = s.docker()?;
    c.args(exec_args(&s.meta, o, cmd));
    Ok(c)
}

pub fn screenshot(paths: &Paths, id: &str, out: &Path) -> Result<()> {
    let s = find(paths, id)?;
    if !s.macos() {
        bail!("only macOS sandboxes have a screen");
    }
    let ip = s.guest(paths)?;
    crate::macos::screenshot(paths, &ip, out)
}

pub fn input(paths: &Paths, id: &str, action: &[String]) -> Result<()> {
    let s = find(paths, id)?;
    if !s.macos() {
        bail!("only macOS sandboxes take input");
    }
    let ip = s.guest(paths)?;
    crate::macos::input(paths, &ip, action)
}

/// Whether the Mac's state of a path (`now`) is still what the sandbox copied (`then`). A
/// directory counts as unchanged whatever its size and mtime; what's in it is checked path
/// by path. Manifests from before modes were recorded don't compare them.
fn unchanged(now: Option<State>, then: Option<State>) -> bool {
    match (now, then) {
        (None, None) => true,
        (Some(n), Some(t)) if n.kind() == 'd' && t.kind() == 'd' => true,
        (Some(n), Some(t)) => (n.0, n.1, n.2) == (t.0, t.1, t.2) && t.3.is_none_or(|m| n.3 == Some(m)),
        _ => false,
    }
}

/// Whether `p` was a directory when the sandbox copied the project. Older manifests list
/// files only, so there a directory shows as the paths under it.
fn was_dir(m: &Manifest, p: &str) -> bool {
    let prefix = format!("{p}/");
    m.get(p).is_some_and(|s| s.kind() == 'd') || m.range(prefix.clone()..).next().is_some_and(|(k, _)| k.starts_with(&prefix))
}

/// Changed paths whose state on the Mac differs from when the sandbox copied them. Without a
/// manifest (sandboxes made before there was one), anything modified after `since`.
fn conflicts(project: &Path, manifest: Option<&Manifest>, since: u64, ch: &[Change]) -> Vec<String> {
    let dir = Dir::open(project).ok();
    ch.iter()
        .filter(|c| match (manifest, &dir) {
            (Some(m), Some(d)) => {
                let now = d.state(Path::new(&c.path));
                // A directory the workspace replaced with a file or link. What was in it
                // shows up as deletions, each checked on its own.
                let replaced = c.kind != 'D' && now.is_some_and(|s| s.kind() == 'd') && was_dir(m, &c.path);
                !replaced && !unchanged(now, m.get(&c.path).copied())
            }
            _ => std::fs::symlink_metadata(project.join(&c.path))
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) > since),
        })
        .map(|c| c.path.clone())
        .collect()
}

/// Why the symlink `link -> target` would leave the tree or reach into `.git`, resolving
/// the links on the way one by one with `read_link` (None for anything that isn't a link).
/// A name that isn't a link counts as a directory, since one may be made there later.
fn leaves(link: &Path, target: &Path, read_link: &dyn Fn(&Path) -> Option<PathBuf>) -> Option<&'static str> {
    enum Part {
        Root,
        Up,
        Name(std::ffi::OsString),
    }
    fn parts(p: &Path) -> Vec<Part> {
        p.components()
            .filter_map(|c| match c {
                Component::RootDir | Component::Prefix(_) => Some(Part::Root),
                Component::ParentDir => Some(Part::Up),
                Component::CurDir => None,
                Component::Normal(n) => Some(Part::Name(n.to_os_string())),
            })
            .collect()
    }
    let mut at: Vec<std::ffi::OsString> = link.parent().map(|p| p.iter().map(|n| n.to_os_string()).collect()).unwrap_or_default();
    let mut todo = parts(target);
    todo.reverse();
    let mut hops = 0;
    while let Some(part) = todo.pop() {
        match part {
            Part::Root => return Some("it points outside the project"),
            Part::Up => {
                if at.pop().is_none() {
                    return Some("it points outside the project");
                }
            }
            Part::Name(n) => {
                if is_git(&n) {
                    return Some("it points into .git");
                }
                at.push(n);
                if let Some(t) = read_link(&at.iter().collect::<PathBuf>()) {
                    hops += 1;
                    if hops > 32 {
                        return Some("its links go round in a loop");
                    }
                    at.pop();
                    todo.extend(parts(&t).into_iter().rev());
                }
            }
        }
    }
    None
}

/// Refuses a symlink that leaves the project or reaches into `.git` in the tree `apply`
/// leaves behind, so `b -> .` with `leak -> b/../outside` is caught although each looks
/// fine alone. That covers links the project already had, in case a new one redirects
/// them; one that already left the project before this apply is the Mac user's own.
fn check_links(dir: &Dir, from: &Dir, src: &Path, ch: &[Change]) -> Result<()> {
    let written: std::collections::HashSet<&Path> = ch.iter().filter(|c| c.kind != 'D').map(|c| Path::new(&c.path)).collect();
    // Afterwards, paths under skipped directories are the project's and the rest are `src`'s.
    let after = |p: &Path| if p.iter().any(skipped) { dir.read_link(p).ok() } else { from.read_link(p).ok() };
    let before = |p: &Path| dir.read_link(p).ok();
    for (p, _) in walk(src).into_iter().filter(|(_, k)| *k == 'l') {
        let target = from.read_link(&p)?;
        let Some(why) = leaves(&p, &target, &after) else { continue };
        let theirs = !written.contains(p.as_path()) && dir.read_link(&p).ok().as_ref() == Some(&target);
        if !theirs || leaves(&p, &target, &before).is_none() {
            bail!("refusing symlink {} -> {}: {why}; nothing was applied", p.display(), target.display());
        }
    }
    Ok(())
}

/// Directories in the project the workspace replaced with a file or symlink, each directory
/// under them before its parent. Refuses one holding anything `apply` doesn't delete
/// (a skipped directory such as `node_modules`, a FIFO).
fn replaced_dirs(dir: &Dir, ch: &[Change]) -> Result<Vec<PathBuf>> {
    let deleted: std::collections::HashSet<&Path> = ch.iter().filter(|c| c.kind == 'D').map(|c| Path::new(&c.path)).collect();
    let mut out = Vec::new();
    for c in ch.iter().filter(|c| c.kind != 'D') {
        let rel = Path::new(&c.path);
        if dir.state(rel).is_none_or(|s| s.kind() != 'd') {
            continue;
        }
        let mut tree = dir.tree(rel)?;
        if let Some((p, _)) = tree.iter().find(|(p, k)| *k != 'd' && !deleted.contains(p.as_path())) {
            bail!("{} is a directory in the project holding {}, which apply doesn't remove; nothing was applied", c.path, p.display());
        }
        tree.reverse();
        out.extend(tree.into_iter().filter(|(_, k)| *k == 'd').map(|(p, _)| p));
        out.push(rel.to_path_buf());
    }
    Ok(out)
}

/// Writes `ch` from the frozen copy `from` (at `src`) into the project `dir`, updating
/// `manifest` for what it wrote. Everything policy refuses is found before anything is
/// written, so an apply is all or nothing as far as policy goes (an I/O error halfway can
/// still leave it partial). Deletions go first, so a file can take a directory's place.
fn write_changes(dir: &Dir, from: &Dir, src: &Path, ch: &[Change], mut manifest: Option<&mut Manifest>) -> Result<()> {
    if let Some(c) = ch.iter().find(|c| Path::new(&c.path).iter().any(is_git)) {
        bail!("refusing {}: it's inside .git; nothing was applied", c.path);
    }
    check_links(dir, from, src, ch)?;
    let replaced = replaced_dirs(dir, ch)?;
    let mut record = |rel: &Path| {
        // What's there now is ours; a later apply mustn't call it a conflict.
        if let Some(m) = manifest.as_mut() {
            let key = rel.to_string_lossy().to_string();
            match dir.state(rel) {
                Some(st) => m.insert(key, st),
                None => m.remove(&key),
            };
        }
    };
    for c in ch.iter().filter(|c| c.kind == 'D') {
        dir.remove(Path::new(&c.path))?;
        record(Path::new(&c.path));
    }
    for d in &replaced {
        dir.remove_dir(d)?;
        record(d);
    }
    for c in ch.iter().filter(|c| c.kind != 'D') {
        let rel = Path::new(&c.path);
        match from.state(rel).map(|s| s.kind()) {
            Some('l') => dir.write_symlink(rel, &from.read_link(rel)?)?,
            // Anything else must be a regular file reached without a symlink.
            _ => dir.write_file(rel, from.open_file(rel)?)?,
        }
        record(rel);
    }
    Ok(())
}

/// Writes the workspace's changes into the real project. Returns what changed. With
/// `reviewed`, writes the copy the last `diff` froze instead of the live workspace.
pub fn apply(paths: &Paths, id: &str, check: bool, reviewed: bool) -> Result<Vec<Change>> {
    let (s, _lock) = copy_sandbox(paths, id)?;
    let src = if reviewed {
        let r = s.paths.home.join("reviewed");
        if !r.is_dir() {
            bail!("nothing reviewed yet; run `rekcod sandbox diff {id}` first");
        }
        r
    } else {
        freeze(&s, "applying")?
    };
    let r = apply_from(s, &src, check);
    if !reviewed {
        let _ = std::fs::remove_dir_all(&src);
    }
    r
}

/// `apply` from the frozen copy `src`, with the review lock held.
fn apply_from(mut s: Sandbox, src: &Path, check: bool) -> Result<Vec<Change>> {
    let ch = changes(&s.meta.project, src);
    let mut manifest = s.manifest();
    let bad = conflicts(&s.meta.project, manifest.as_ref(), s.meta.created, &ch);
    if !bad.is_empty() {
        bail!("changed in {} since the sandbox copied it: {}", s.meta.project.display(), bad.join(", "));
    }
    if check {
        return Ok(ch);
    }
    let mut dir = Dir::open(&s.meta.project)?;
    dir.protect(".git");
    write_changes(&dir, &Dir::open(src)?, src, &ch, manifest.as_mut())?;
    match &manifest {
        Some(m) => s.save_manifest(m)?,
        None => {
            s.meta.created = now() + 1;
            s.save()?;
        }
    }
    Ok(ch)
}

pub fn print_changes(ch: &[Change]) {
    for c in ch {
        println!("{} {}", c.kind, c.path);
    }
}

pub fn ports(paths: &Paths, id: &str, json: bool) -> Result<()> {
    let s = find(paths, id)?;
    if s.macos() {
        bail!("macOS sandboxes don't forward ports; reach the guest at the address `rekcod sandbox ls --json` shows");
    }
    crate::cli::status(&s.paths, json)
}

pub fn log(paths: &Paths, id: &str, follow: bool, json: bool, net: bool) -> Result<()> {
    let s = find(paths, id)?;
    if net {
        // Lookups the guest agent refused (console), then lookups and connections the
        // daemon refused (docs/sandbox-network.md).
        for log in [s.paths.console_log(), s.paths.daemon_log()] {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            for l in text.lines().filter(|l| l.contains("rekcod-egress:")) {
                println!("{l}");
            }
        }
        return Ok(());
    }
    let file = s.paths.home.join("log.jsonl");
    if follow {
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&file);
        return Err(Command::new("tail").args(["-n", "+1", "-f"]).arg(&file).exec()).context("exec tail");
    }
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    for l in text.lines() {
        if json {
            println!("{l}");
        } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
            let cmd: Vec<&str> = v["cmd"].as_array().into_iter().flatten().filter_map(|c| c.as_str()).collect();
            println!("{} exit {:<3} {:>6} ms  {}", v["ts"], v["exit"], v["ms"], cmd.join(" "));
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub meta: Meta,
    pub running: bool,
    /// macOS guests: the address SSH and the screen tools use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The VM process, while it runs (CPU and memory in `rekcod state`).
    #[serde(skip)]
    pub pid: Option<i32>,
    /// The sandbox's state directory.
    #[serde(skip)]
    pub home: PathBuf,
}

fn listed(s: Sandbox) -> Listed {
    let running = s.running();
    let address = (s.macos() && running)
        .then(|| std::fs::read(s.paths.home.join("config.json")).ok())
        .flatten()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["mac"].as_str().and_then(crate::macos::running_ip));
    let pid = if running { crate::cli::running_pid(&s.paths) } else { None };
    Listed { running, address, pid, home: s.paths.home, meta: s.meta }
}

/// Running macOS sandboxes (Apple's licence allows two per Mac).
pub fn macos_running(paths: &Paths) -> usize {
    all(paths).iter().filter(|s| s.macos() && s.running()).count()
}

pub fn list(paths: &Paths) -> Vec<Listed> {
    all(paths).into_iter().map(listed).collect()
}

pub fn get(paths: &Paths, id: &str) -> Result<Listed> {
    Ok(listed(find(paths, id)?))
}

pub fn print_list(paths: &Paths, json: bool) -> Result<()> {
    let l = list(paths);
    if json {
        println!("{}", serde_json::to_string_pretty(&l)?);
        return Ok(());
    }
    println!("{:<9} {:<14} {:<8} {:<5} PROJECT", "ID", "NAME", "STATE", "MODE");
    for s in l {
        let m = &s.meta;
        let state = if s.running { "running" } else { "stopped" };
        let mode = if m.os == "macos" { "macOS" } else { m.mode.as_str() };
        println!("{:<9} {:<14} {:<8} {:<5} {}", m.id, m.name.as_deref().unwrap_or("-"), state, mode, m.project.display());
    }
    Ok(())
}

pub fn stop(paths: &Paths, id: &str) -> Result<()> {
    crate::cli::stop_daemon(&find(paths, id)?.paths).map(|_| ())
}

fn remove(s: &Sandbox) -> Result<()> {
    crate::cli::signal_daemon(&s.paths, libc::SIGUSR1)?;
    // The daemon deletes its own run files as it exits, which can race the walk here.
    let mut r = std::fs::remove_dir_all(&s.paths.home);
    if r.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        r = std::fs::remove_dir_all(&s.paths.home).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) });
    }
    r.with_context(|| format!("remove {}", s.paths.home.display()))
}

pub fn rm(paths: &Paths, ids: &[String], every: bool) -> Result<()> {
    let targets: Vec<Sandbox> = if every { all(paths) } else { ids.iter().map(|i| find(paths, i)).collect::<Result<_>>()? };
    for s in targets {
        remove(&s)?;
        println!("{}", s.meta.id);
    }
    Ok(())
}

pub fn golden_build(paths: &Paths, name: &str, images: &[String]) -> Result<()> {
    check_name("golden disk", name)?;
    let home = golden_home(paths, name);
    let g = Paths::at(home.clone())?;
    crate::cli::exclude_from_backups(&paths.home.join("golden"));
    if crate::cli::running_pid(&g).is_some() {
        bail!("golden disk {name} is already being built");
    }
    // Sandbox-style boot: it shares an empty directory, not the Mac's home.
    let empty = home.join("empty");
    std::fs::create_dir_all(&empty)?;
    let opts = crate::cli::BootOptions {
        cpus: Some(4),
        memory_gib: Some(4),
        disk_gib: DISK_GIB,
        project: Some(empty.canonicalize()?),
        guest_dir: Some(paths.guest_dir()?),
        ..Default::default()
    };
    crate::cli::boot(&g, &opts)?;
    let images: Vec<String> = if images.is_empty() { GOLDEN_IMAGES.iter().map(|s| s.to_string()).collect() } else { images.to_vec() };
    let mut result = Ok(());
    for img in &images {
        eprintln!("rekcod: pulling {img}");
        let mut c = crate::docker::any_cli(paths)?;
        if !c.env("DOCKER_HOST", g.docker_host()).args(["pull", "-q", img]).status()?.success() {
            result = Err(anyhow::anyhow!("pull {img} failed"));
            break;
        }
    }
    crate::cli::stop_daemon(&g)?;
    result?;
    std::fs::write(home.join("images"), images.join("\n") + "\n")?;
    println!("golden disk {name}: {}", images.join(", "));
    Ok(())
}

pub fn golden_list(paths: &Paths) -> Result<()> {
    for e in std::fs::read_dir(paths.home.join("golden")).into_iter().flatten().flatten() {
        if e.path().join("vm/data.img").exists() {
            let images = std::fs::read_to_string(e.path().join("images")).unwrap_or_default();
            println!("{:<12} {}", e.file_name().to_string_lossy(), images.split_whitespace().collect::<Vec<_>>().join(", "));
        }
    }
    Ok(())
}

pub fn golden_rm(paths: &Paths, name: &str) -> Result<()> {
    check_name("golden disk", name)?;
    let home = golden_home(paths, name);
    if !home.exists() {
        bail!("no golden disk {name}");
    }
    let _ = crate::cli::stop_daemon(&Paths { home: home.clone() });
    std::fs::remove_dir_all(home)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_base32() {
        let id = new_id().unwrap();
        assert_eq!(id.len(), 8);
        assert!(id.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)), "{id}");
        assert_ne!(id, new_id().unwrap());
    }

    #[test]
    fn allowlist_matches_names_and_wildcards() {
        let allow = allowlist(&["example.com".into()]).unwrap();
        assert!(host_allowed(&allow, "registry.npmjs.org."));
        assert!(host_allowed(&allow, "Registry-1.Docker.IO"));
        assert!(host_allowed(&allow, "example.com"));
        assert!(!host_allowed(&allow, "evil.example.com"));
        assert!(!host_allowed(&allow, "docker.io.evil.net"));
        assert!(!host_allowed(&allow, "gist.github.com.evil"));
        assert_eq!(allowlist(&["*".into()]), None);
    }

    #[test]
    fn changes_and_conflicts() {
        let base = std::env::temp_dir().join(format!("rekcod-sb-{}", std::process::id()));
        let (p, w) = (base.join("p"), base.join("w"));
        for d in [&p, &w] {
            std::fs::create_dir_all(d.join("src")).unwrap();
            std::fs::create_dir_all(d.join("node_modules")).unwrap();
            std::fs::write(d.join("same.txt"), "x").unwrap();
            std::fs::write(d.join("node_modules/ignored"), d.to_string_lossy().as_bytes()).unwrap();
        }
        std::fs::write(p.join("src/a.rs"), "old").unwrap();
        std::fs::write(w.join("src/a.rs"), "new").unwrap();
        std::fs::write(p.join("gone.txt"), "x").unwrap();
        std::fs::write(w.join("src/b.rs"), "added").unwrap();
        let ch = changes(&p, &w);
        let got: Vec<(char, &str)> = ch.iter().map(|c| (c.kind, c.path.as_str())).collect();
        assert_eq!(got, vec![('D', "gone.txt"), ('M', "src/a.rs"), ('A', "src/b.rs")]);
        // No manifest: everything in p was just written, so it all counts as changed after an old copy.
        assert_eq!(conflicts(&p, None, 0, &ch).len(), 2);
        assert!(conflicts(&p, None, now() + 10, &ch).is_empty());
        // With one, only paths whose state moved on conflict, whatever their mtime.
        let m = manifest(&p);
        assert!(conflicts(&p, Some(&m), 0, &ch).is_empty());
        std::fs::write(p.join("src/a.rs"), "edited on the Mac").unwrap();
        // A file moved in with an old mtime still conflicts: it wasn't there when copied.
        std::fs::write(p.join("src/b.rs"), "moved in").unwrap();
        let got = conflicts(&p, Some(&m), now() + 10, &ch);
        assert_eq!(got, vec!["src/a.rs", "src/b.rs"]);
        // FIFOs are left out of the walk rather than read.
        let fifo = std::ffi::CString::new(w.join("pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        assert!(!changes(&p, &w).iter().any(|c| c.path == "pipe"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn printable_hides_escape_sequences() {
        assert_eq!(printable("ok\n\tx"), "ok\n\tx");
        assert_eq!(printable("a\x1b[2Kb\r"), "a^[[2Kb^M");
        assert_eq!(printable("\u{9b}"), "\\u{9b}");
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("golden disk", "default").is_ok());
        assert!(check_name("golden disk", "node-22.x").is_ok());
        for bad in ["..", "../sandboxes/x", "a/b", "", "-x", "A"] {
            assert!(check_name("golden disk", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exec_args_use_workspace_container() {
        let m = Meta {
            id: "abcdefgh".into(),
            name: None,
            project: "/Users/me/app".into(),
            mode: "copy".into(),
            image: "node:22-slim".into(),
            golden: None,
            allow: None,
            offline: false,
            cpus: 2,
            memory_gib: 2,
            ttl_secs: 0,
            secrets: vec![],
            created: 0,
            os: String::new(),
            reach: vec![],
        };
        let o = ExecOptions { workdir: None, env: vec!["A=1".into()], timeout_secs: Some(5), tty: false };
        let a = exec_args(&m, &o, &["npm".into(), "test".into()]).join(" ");
        assert_eq!(a, "exec -i -w /Users/me/app -e A=1 rekcod-ws timeout -s KILL 5 npm test");
        let back: Meta = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    use std::os::unix::fs::{PermissionsExt, symlink};
    use testing::fake;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rekcod-sbt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    /// A project holding `files`, a stopped copy sandbox of it, and that sandbox's workspace.
    fn setup(name: &str, files: &[(&str, &str)]) -> (Paths, PathBuf, Meta, PathBuf) {
        let base = tmp(name);
        let project = base.join("p");
        for (f, body) in files {
            std::fs::create_dir_all(project.join(f).parent().unwrap()).unwrap();
            std::fs::write(project.join(f), body).unwrap();
        }
        std::fs::create_dir_all(&project).unwrap();
        let paths = Paths { home: base.join("home") };
        let m = fake(&paths, &project, |_| {});
        let ws = root(&paths).join(&m.id).join("ws");
        (paths, project, m, ws)
    }

    fn paths_of(ch: &[Change]) -> Vec<(char, &str)> {
        ch.iter().map(|c| (c.kind, c.path.as_str())).collect()
    }

    #[test]
    fn git_metadata_is_never_applied() {
        for n in [".git", ".GIT", ".Git", ".g\u{200c}it", ".gi\u{feff}t", ".G\u{130}T"] {
            assert!(is_git(std::ffi::OsStr::new(n)), "{n}");
        }
        for n in ["git", ".gitignore", ".github", "x.git"] {
            assert!(!is_git(std::ffi::OsStr::new(n)), "{n}");
        }
        let (paths, project, m, ws) = setup("git", &[("f", "x"), (".git/config", "[core]")]);
        // Case-only renames: on case-insensitive APFS `.GIT` would resolve to the real `.git`.
        std::fs::rename(ws.join(".git"), ws.join(".GIT")).unwrap();
        std::fs::write(ws.join(".GIT/marker"), "hook").unwrap();
        std::fs::create_dir_all(ws.join(".g\u{200c}it")).unwrap();
        std::fs::write(ws.join(".g\u{200c}it/marker"), "hook").unwrap();
        std::fs::write(ws.join("f"), "y").unwrap();
        assert_eq!(paths_of(&patch(&paths, &m.id).unwrap().0), vec![('M', "f")]);
        assert_eq!(paths_of(&apply(&paths, &m.id, false, true).unwrap()), vec![('M', "f")]);
        std::fs::rename(ws.join(".GIT"), ws.join(".Git")).unwrap();
        std::fs::write(ws.join(".Git/marker2"), "hook").unwrap();
        assert!(apply(&paths, &m.id, false, false).unwrap().is_empty());
        assert!(!project.join(".git/marker").exists() && !project.join(".git/marker2").exists());
        assert!(std::fs::symlink_metadata(project.join(".g\u{200c}it")).is_err());
        assert_eq!(std::fs::read_to_string(project.join(".git/config")).unwrap(), "[core]");
        // A change list that names `.git` some other way is refused before anything is written.
        let ch = vec![Change { kind: 'A', path: ".GIT/hooks/pre-commit".into() }];
        let e = write_changes(&Dir::open(&project).unwrap(), &Dir::open(&ws).unwrap(), &ws, &ch, None).unwrap_err();
        assert!(e.to_string().contains("inside .git"), "{e}");
    }

    #[test]
    fn link_resolution_follows_chains() {
        let links: std::collections::HashMap<&str, &str> =
            [("b", "."), ("sub/up", ".."), ("loop", "loop"), ("nm", "/opt/x"), ("deep/l", "../b")].into_iter().collect();
        let rl = |p: &Path| links.get(p.to_str().unwrap()).map(PathBuf::from);
        let leaves = |l: &str, t: &str| leaves(Path::new(l), Path::new(t), &rl);
        assert_eq!(leaves("leak", "b/../outside"), Some("it points outside the project"));
        assert_eq!(leaves("leak", "sub/up/../x"), Some("it points outside the project"));
        assert_eq!(leaves("x", "deep/l/../y"), Some("it points outside the project"));
        assert_eq!(leaves("x", "nm"), Some("it points outside the project"));
        assert_eq!(leaves("x", "loop/a"), Some("its links go round in a loop"));
        assert_eq!(leaves("x", "sub/.GIT/hooks"), Some("it points into .git"));
        // Each of these stays in: `b` is the root, so `b/x` is `x`.
        assert_eq!(leaves("ok", "b/x"), None);
        assert_eq!(leaves("a/ok", "../sub/up/x"), None);
        assert_eq!(leaves("ok", "missing/../x"), None);
    }

    #[test]
    fn apply_refuses_symlink_chains_out_of_the_project() {
        let (paths, project, m, ws) = setup("chains", &[("f", "x")]);
        std::fs::write(project.parent().unwrap().join("outside-marker"), "outside").unwrap();
        symlink(".", ws.join("b")).unwrap();
        symlink("b/../outside-marker", ws.join("leak")).unwrap();
        let e = apply(&paths, &m.id, false, false).unwrap_err().to_string();
        assert!(e.contains("leak") && e.contains("outside the project"), "{e}");
        assert!(std::fs::symlink_metadata(project.join("leak")).is_err() && std::fs::symlink_metadata(project.join("b")).is_err());
        // Split over two applies: the second link redirects the first.
        std::fs::remove_file(ws.join("b")).unwrap();
        assert_eq!(paths_of(&apply(&paths, &m.id, false, false).unwrap()), vec![('A', "leak")]);
        symlink(".", ws.join("b")).unwrap();
        let e = apply(&paths, &m.id, false, false).unwrap_err().to_string();
        assert!(e.contains("leak"), "{e}");
        assert!(std::fs::symlink_metadata(project.join("b")).is_err());
        // Through a link in a directory apply leaves alone.
        std::fs::remove_file(ws.join("b")).unwrap();
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        symlink("../..", project.join("node_modules/up")).unwrap();
        symlink("node_modules/up/outside-marker", ws.join("x")).unwrap();
        let e = apply(&paths, &m.id, false, false).unwrap_err().to_string();
        assert!(e.contains("refusing symlink x"), "{e}");
        // A chain that stays inside is written; one into .git isn't.
        std::fs::remove_file(ws.join("x")).unwrap();
        std::fs::create_dir(ws.join("real")).unwrap();
        std::fs::write(ws.join("real/file"), "r").unwrap();
        symlink("real", ws.join("alias")).unwrap();
        symlink("alias/file", ws.join("via")).unwrap();
        apply(&paths, &m.id, false, false).unwrap();
        assert_eq!(std::fs::read_to_string(project.join("via")).unwrap(), "r");
        symlink("alias/../.GIT/hooks", ws.join("hooks")).unwrap();
        let e = apply(&paths, &m.id, false, false).unwrap_err().to_string();
        assert!(e.contains("into .git"), "{e}");
    }

    #[test]
    fn apply_writes_exactly_what_was_reviewed() {
        let (paths, project, m, ws) = setup("review", &[("f", "old")]);
        std::fs::write(ws.join("f"), "reviewed-A").unwrap();
        assert_eq!(paths_of(&patch(&paths, &m.id).unwrap().0), vec![('M', "f")]);
        std::fs::write(ws.join("f"), "unreviewed-B").unwrap();
        apply(&paths, &m.id, false, true).unwrap();
        assert_eq!(std::fs::read_to_string(project.join("f")).unwrap(), "reviewed-A");
        // A diff waits for an apply (or another diff) holding the sandbox's lock.
        let home = root(&paths).join(&m.id);
        let lock = review_lock(&home).unwrap();
        let (p, id) = (paths.clone(), m.id.clone());
        let t = std::thread::spawn(move || patch(&p, &id).map(|(ch, _)| ch.len()));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!t.is_finished());
        assert_eq!(std::fs::read_to_string(home.join("reviewed/f")).unwrap(), "reviewed-A");
        drop(lock);
        assert_eq!(t.join().unwrap().unwrap(), 1);
        assert_eq!(std::fs::read_to_string(home.join("reviewed/f")).unwrap(), "unreviewed-B");
        // The copy is read without following links: a directory swapped for a link, inside
        // the copy or out to a Mac path, is never read through.
        let (inside, outside) = (tmp("review-inside"), tmp("review-outside"));
        std::fs::write(outside.join("secret"), "host only").unwrap();
        std::fs::create_dir(inside.join("sub")).unwrap();
        std::fs::write(inside.join("sub/secret"), "elsewhere").unwrap();
        symlink("sub", inside.join("z-dir")).unwrap();
        let mac = tmp("review-mac");
        symlink(&outside, mac.join("z-dir")).unwrap();
        let dir = Dir::open(&project).unwrap();
        let ch = vec![Change { kind: 'A', path: "z-dir/secret".into() }];
        for src in [&inside, &mac] {
            assert!(write_changes(&dir, &Dir::open(src).unwrap(), src, &ch, None).is_err(), "{}", src.display());
        }
        assert!(std::fs::symlink_metadata(project.join("z-dir")).is_err());
    }

    #[test]
    fn apply_compares_and_writes_modes() {
        let (paths, project, m, ws) = setup("mode", &[("f", "x"), ("g", "y")]);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let was = mode(&project.join("f"));
        std::fs::set_permissions(project.join("f"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let e = apply(&paths, &m.id, true, false).unwrap_err().to_string();
        assert!(e.ends_with("since the sandbox copied it: f"), "{e}");
        assert_eq!(mode(&project.join("f")), 0o600);
        std::fs::set_permissions(project.join("f"), std::fs::Permissions::from_mode(was)).unwrap();
        assert!(apply(&paths, &m.id, true, false).unwrap().is_empty());
        std::fs::set_permissions(ws.join("g"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(paths_of(&apply(&paths, &m.id, false, false).unwrap()), vec![('M', "g")]);
        assert_eq!(mode(&project.join("g")), 0o755);
        assert!(apply(&paths, &m.id, true, false).unwrap().is_empty());
    }

    #[test]
    fn apply_replaces_a_directory_with_a_file() {
        let (paths, project, m, ws) = setup("dir2file", &[("a/child", "x"), ("a/sub/deep", "y"), ("keep", "k")]);
        // A manifest from before directories and modes were recorded.
        let file = root(&paths).join(&m.id).join("manifest.json");
        let new: Manifest = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let old: std::collections::BTreeMap<&String, (char, u64, i128)> =
            new.iter().filter(|(_, s)| s.kind() != 'd').map(|(k, s)| (k, (s.0, s.1, s.2))).collect();
        std::fs::write(&file, serde_json::to_vec(&old).unwrap()).unwrap();
        std::fs::remove_dir_all(ws.join("a")).unwrap();
        std::fs::write(ws.join("a"), "a file now").unwrap();
        assert_eq!(paths_of(&patch(&paths, &m.id).unwrap().0), vec![('A', "a"), ('D', "a/child"), ('D', "a/sub/deep")]);
        apply(&paths, &m.id, false, true).unwrap();
        assert_eq!(std::fs::read_to_string(project.join("a")).unwrap(), "a file now");
        // And back: the file replaced by a directory.
        std::fs::remove_file(ws.join("a")).unwrap();
        std::fs::create_dir(ws.join("a")).unwrap();
        std::fs::write(ws.join("a/x"), "dir again").unwrap();
        apply(&paths, &m.id, false, false).unwrap();
        assert_eq!(std::fs::read_to_string(project.join("a/x")).unwrap(), "dir again");
        // A conflict only when the Mac changed something under the directory.
        let (paths, project, m, ws) = setup("dir2file-host", &[("d/child", "x"), ("e/child", "x"), ("e/node_modules/pkg", "p")]);
        std::fs::remove_dir_all(ws.join("d")).unwrap();
        std::fs::write(ws.join("d"), "file").unwrap();
        std::fs::write(project.join("d/new"), "made on the Mac").unwrap();
        let e = apply(&paths, &m.id, false, false).unwrap_err().to_string();
        assert!(e.ends_with("since the sandbox copied it: d/new"), "{e}");
        std::fs::remove_file(project.join("d/new")).unwrap();
        // One holding what apply leaves alone (node_modules) is refused before anything is written.
        std::fs::remove_dir_all(ws.join("e")).unwrap();
        std::fs::write(ws.join("e"), "file").unwrap();
        let e = apply(&paths, &m.id, false, false).unwrap_err().to_string();
        assert!(e.contains("e/node_modules"), "{e}");
        assert!(project.join("d/child").exists() && project.join("e/child").exists());
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A stopped copy-mode sandbox of `project` in `paths`, laid out as `create` leaves
    /// one, with no VM. `edit` changes its metadata before it's saved.
    pub fn fake(paths: &Paths, project: &Path, edit: impl FnOnce(&mut Meta)) -> Meta {
        let id = new_id().unwrap();
        let home = root(paths).join(&id);
        std::fs::create_dir_all(&home).unwrap();
        let mut meta = Meta {
            id,
            name: None,
            project: project.canonicalize().unwrap(),
            mode: "copy".into(),
            image: DEFAULT_IMAGE.into(),
            golden: None,
            allow: allowlist(&[]),
            offline: false,
            cpus: 2,
            memory_gib: 2,
            ttl_secs: 0,
            secrets: vec![],
            created: now(),
            os: String::new(),
            reach: vec![],
        };
        edit(&mut meta);
        let s = Sandbox { meta, paths: Paths { home } };
        clonefile(&s.meta.project, &s.ws()).unwrap();
        s.save_manifest(&manifest(&s.meta.project)).unwrap();
        s.save().unwrap();
        s.meta
    }
}
