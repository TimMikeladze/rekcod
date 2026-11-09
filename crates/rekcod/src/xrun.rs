//! `rekcod x <cmd>`: run a command in a container with no Dockerfile
//! (docs/phase5.md §6-7, plan.md §3b).
//!
//! The image comes from the command or the project's version files, the
//! current directory is mounted at the same path, Linux dependencies live on
//! VM-local volumes, and listening ports show up on the Mac's localhost
//! (the daemon's autoports). `--sandbox` runs it in a separate VM that sees
//! only this directory (docs/sandbox.md).

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::ports::AUTOPORTS_LABEL;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Node,
    Bun,
    Deno,
    Python,
    Go,
    Rust,
    Ruby,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub image: String,
    /// Project-relative dirs kept on VM-local volumes (Linux builds of deps).
    pub local_dirs: Vec<&'static str>,
    /// (volume, container path) shared across projects (package caches).
    pub caches: Vec<(&'static str, &'static str)>,
    /// Wrap the command to enable corepack (pnpm, yarn).
    pub corepack: bool,
}

fn runtime_of_command(cmd: &str) -> Option<Runtime> {
    let base = Path::new(cmd).file_name()?.to_str()?;
    Some(match base {
        "node" | "npm" | "npx" | "pnpm" | "pnpx" | "yarn" | "corepack" => Runtime::Node,
        "bun" | "bunx" => Runtime::Bun,
        "deno" => Runtime::Deno,
        "python" | "python3" | "pip" | "pip3" | "uv" | "uvx" | "pytest" | "poetry" => Runtime::Python,
        "go" | "gofmt" => Runtime::Go,
        "cargo" | "rustc" | "rustup" => Runtime::Rust,
        "ruby" | "gem" | "bundle" | "bundler" | "rails" | "rake" | "irb" => Runtime::Ruby,
        _ => return None,
    })
}

/// Largest project file read on the Mac (version files, `rekcod.toml`).
pub const MAX_PROJECT_FILE: u64 = 1 << 20;

/// Reads `name`, a file directly in `dir`, without following a symlink: only a regular file
/// of at most [`MAX_PROJECT_FILE`] bytes. `Ok(None)` when there's nothing by that name.
/// Fleet nodes read cloned repositories with this, so a link to `/dev/zero` or to a file
/// outside the checkout is an error, not a read.
pub fn read_project_file(dir: &Path, name: &str) -> Result<Option<String>> {
    read_file(dir, name, false)
}

/// [`read_project_file`], following a symlink when `follow`; the target must still be a
/// regular file of at most [`MAX_PROJECT_FILE`] bytes.
fn read_file(dir: &Path, name: &str, follow: bool) -> Result<Option<String>> {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        bail!("{name}: not a file name");
    }
    let d = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(dir)
        .with_context(|| format!("open {}", dir.display()))?;
    let c = std::ffi::CString::new(name)?;
    // O_NONBLOCK: a FIFO would otherwise block the open until something writes to it.
    let nofollow = if follow { 0 } else { libc::O_NOFOLLOW };
    let fd = unsafe { libc::openat(d.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | nofollow | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::ENOENT) => Ok(None),
            Some(libc::ELOOP) => bail!("{name} is a symlink; only regular files in the repository are read"),
            _ => Err(anyhow::Error::new(e).context(format!("open {name}"))),
        };
    }
    let f = unsafe { std::fs::File::from_raw_fd(fd) };
    let m = f.metadata()?;
    if !m.is_file() {
        bail!("{name} isn't a regular file");
    }
    let mut b = Vec::new();
    f.take(MAX_PROJECT_FILE + 1).read_to_end(&mut b)?;
    if b.len() as u64 > MAX_PROJECT_FILE {
        bail!("{name} is over {} KiB", MAX_PROJECT_FILE >> 10);
    }
    Ok(Some(String::from_utf8(b).with_context(|| format!("{name} isn't UTF-8"))?))
}

/// A regular file `name` is directly in `dir` (a symlink doesn't count).
pub fn has_project_file(dir: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(dir.join(name)).is_ok_and(|m| m.is_file())
}

/// A project's files as [`plan`] reads them. Repositories rekcod cloned (fleet checkouts,
/// all under the rekcod home) are read without following symlinks; the Mac owner's own
/// projects follow them, since monorepos link `.nvmrc` to a parent's.
struct Project<'a> {
    dir: &'a Path,
    follow: bool,
}

impl Project<'_> {
    fn read(&self, name: &str) -> Option<String> {
        read_file(self.dir, name, self.follow).ok().flatten()
    }

    fn has(&self, name: &str) -> bool {
        if self.follow { std::fs::metadata(self.dir.join(name)).is_ok_and(|m| m.is_file()) } else { has_project_file(self.dir, name) }
    }
}

/// `dir` isn't under the rekcod home, where everything rekcod cloned lives.
fn users_own(dir: &Path) -> bool {
    let Ok(home) = crate::paths::home_dir() else { return false };
    let home = home.canonicalize().unwrap_or(home);
    !dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()).starts_with(home)
}

/// A version like `v22.3.1`, `22`, `>=20 <23`, `^3.12`, `lts/*`: major[.minor] or None.
fn clean_version(raw: &str, keep_minor: bool) -> Option<String> {
    let s = raw.trim();
    if s.starts_with("lts") {
        return Some("lts".into());
    }
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let v: String = s[start..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = v.split('.').filter(|p| !p.is_empty());
    let major = parts.next()?;
    Some(match (keep_minor, parts.next()) {
        (true, Some(minor)) => format!("{major}.{minor}"),
        _ => major.to_string(),
    })
}

/// `.tool-versions` (asdf) and `mise.toml` entries: (tool, version).
fn tool_versions(p: &Project) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(text) = p.read(".tool-versions") {
        for line in text.lines() {
            let mut f = line.split_whitespace();
            if let (Some(t), Some(v)) = (f.next(), f.next())
                && !t.starts_with('#')
            {
                out.push((t.to_string(), v.to_string()));
            }
        }
    }
    if let Some(text) = p.read("mise.toml").or_else(|| p.read(".mise.toml")) {
        let mut in_tools = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_tools = line == "[tools]";
            } else if in_tools && let Some((k, v)) = line.split_once('=') {
                out.push((k.trim().trim_matches('"').to_string(), v.trim().trim_matches(['"', '\'']).to_string()));
            }
        }
    }
    out
}

fn runtime_of_tool(tool: &str) -> Option<Runtime> {
    Some(match tool {
        "node" | "nodejs" => Runtime::Node,
        "bun" => Runtime::Bun,
        "deno" => Runtime::Deno,
        "python" => Runtime::Python,
        "go" | "golang" => Runtime::Go,
        "rust" => Runtime::Rust,
        "ruby" => Runtime::Ruby,
        _ => return None,
    })
}

/// The runtime a project's files point to, in the order of docs/phase5.md §6.
fn runtime_of_project(p: &Project) -> Option<Runtime> {
    if let Some(r) = tool_versions(p).iter().find_map(|(t, _)| runtime_of_tool(t)) {
        return Some(r);
    }
    let has = |n: &str| p.has(n);
    if has(".bun-version") || has("bun.lock") || has("bun.lockb") {
        return Some(Runtime::Bun);
    }
    if has(".nvmrc") || has(".node-version") || has("package.json") {
        return Some(Runtime::Node);
    }
    if has("deno.json") || has("deno.jsonc") {
        return Some(Runtime::Deno);
    }
    if has(".python-version") || has("pyproject.toml") || has("requirements.txt") || has("uv.lock") {
        return Some(Runtime::Python);
    }
    if has("go.mod") {
        return Some(Runtime::Go);
    }
    if has("Cargo.toml") || has("rust-toolchain.toml") || has("rust-toolchain") {
        return Some(Runtime::Rust);
    }
    if has("Gemfile") || has(".ruby-version") {
        return Some(Runtime::Ruby);
    }
    None
}

/// The version the project pins for `rt`, if any.
fn version_for(p: &Project, rt: Runtime) -> Option<String> {
    let from_tools = tool_versions(p)
        .into_iter()
        .find(|(t, _)| runtime_of_tool(t) == Some(rt))
        .and_then(|(_, v)| clean_version(&v, matches!(rt, Runtime::Python | Runtime::Go)));
    if from_tools.is_some() {
        return from_tools;
    }
    match rt {
        Runtime::Node => p.read(".nvmrc").or_else(|| p.read(".node-version")).and_then(|v| clean_version(&v, false)).or_else(|| {
            let pkg: serde_json::Value = serde_json::from_str(&p.read("package.json")?).ok()?;
            clean_version(pkg["engines"]["node"].as_str()?, false)
        }),
        Runtime::Bun => p.read(".bun-version").and_then(|v| clean_version(&v, true)),
        Runtime::Python => p.read(".python-version").and_then(|v| clean_version(&v, true)).or_else(|| {
            let text = p.read("pyproject.toml")?;
            let line = text.lines().find(|l| l.trim_start().starts_with("requires-python"))?;
            clean_version(line.split_once('=')?.1, true)
        }),
        Runtime::Go => {
            let text = p.read("go.mod")?;
            let line = text.lines().find(|l| l.starts_with("go "))?;
            clean_version(&line[3..], true)
        }
        Runtime::Rust => {
            let text = p.read("rust-toolchain.toml").or_else(|| p.read("rust-toolchain"))?;
            let line = text.lines().find(|l| l.contains("channel")).unwrap_or(text.lines().next()?);
            clean_version(line, true)
        }
        Runtime::Ruby => p.read(".ruby-version").and_then(|v| clean_version(&v, true)),
        Runtime::Deno => None,
    }
}

/// Decides image, dependency dirs and caches for `cmd` run in `dir`.
pub fn plan(dir: &Path, cmd: &[String]) -> Option<Plan> {
    plan_in(&Project { dir, follow: users_own(dir) }, cmd)
}

fn plan_in(p: &Project, cmd: &[String]) -> Option<Plan> {
    let first = cmd.first().map(String::as_str).unwrap_or_default();
    let rt = runtime_of_command(first).or_else(|| runtime_of_project(p))?;
    let v = version_for(p, rt);
    let uses_uv = first == "uv" || first == "uvx" || p.has("uv.lock");
    let image = match rt {
        Runtime::Node => format!("node:{}-slim", v.unwrap_or_else(|| "lts".into())),
        Runtime::Bun => format!("oven/bun:{}", v.unwrap_or_else(|| "1".into())),
        Runtime::Deno => "denoland/deno:latest".into(),
        Runtime::Python if uses_uv => format!("ghcr.io/astral-sh/uv:python{}-bookworm-slim", v.unwrap_or_else(|| "3.14".into())),
        Runtime::Python => format!("python:{}-slim", v.unwrap_or_else(|| "3.14".into())),
        Runtime::Go => format!("golang:{}", v.unwrap_or_else(|| "1".into())),
        Runtime::Rust => format!("rust:{}", v.unwrap_or_else(|| "1".into())),
        Runtime::Ruby => format!("ruby:{}-slim", v.unwrap_or_else(|| "3".into())),
    };
    let (local_dirs, caches) = match rt {
        Runtime::Node => (vec!["node_modules"], vec![("rekcod-x-cache-npm", "/root/.npm"), ("rekcod-x-cache-node", "/root/.cache")]),
        Runtime::Bun => (vec!["node_modules"], vec![("rekcod-x-cache-bun", "/root/.bun/install/cache")]),
        Runtime::Python => (vec![".venv"], vec![("rekcod-x-cache-pip", "/root/.cache")]),
        Runtime::Go => (vec![], vec![("rekcod-x-cache-go", "/go/pkg/mod")]),
        Runtime::Rust => (vec!["target"], vec![("rekcod-x-cache-cargo", "/usr/local/cargo/registry")]),
        Runtime::Ruby => (vec![], vec![("rekcod-x-cache-gems", "/usr/local/bundle")]),
        Runtime::Deno => (vec![], vec![("rekcod-x-cache-deno", "/deno-dir")]),
    };
    let corepack = matches!(first, "pnpm" | "pnpx" | "yarn");
    Some(Plan { image, local_dirs, caches, corepack })
}

/// Stable per-project volume name for a dependency dir.
pub fn local_volume(project: &Path, dir: &str) -> String {
    let hash =
        project.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let base: String = project
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let dir: String = dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("rekcod-x_{}_{:08x}_{}", base.trim_matches('-'), hash as u32, dir.trim_matches('-'))
}

pub struct Options {
    pub image: Option<String>,
    pub publish: Vec<String>,
    pub env: Vec<String>,
    pub no_autoports: bool,
    pub sandbox: bool,
    pub offline: bool,
    pub secrets: Vec<String>,
    pub keep: bool,
}

/// The `docker run` arguments (everything after `docker`).
pub fn docker_args(project: &Path, cmd: &[String], plan: &Plan, opts: &Options, tty: bool) -> Vec<String> {
    let p = project.to_string_lossy().to_string();
    let mut a: Vec<String> = vec!["run".into(), "--rm".into(), "-i".into()];
    if tty {
        a.push("-t".into());
    }
    a.extend(["--name".into(), format!("rekcod-x-{}", std::process::id())]);
    a.extend(["--label".into(), format!("dev.rekcod.x={p}")]);
    if opts.offline {
        a.extend(["--network".into(), "none".into()]);
    } else if !opts.publish.is_empty() || opts.no_autoports {
        for port in &opts.publish {
            a.extend(["-p".into(), if port.contains(':') { port.clone() } else { format!("127.0.0.1:{port}:{port}") }]);
        }
    } else {
        // The VM's network: listeners are on the VM's 127.0.0.1, where the
        // daemon's port forwarding dials (docs/phase5.md §6).
        a.extend(["--network".into(), "host".into(), "--label".into(), format!("{AUTOPORTS_LABEL}=1")]);
        if !opts.sandbox {
            a.extend(["--add-host".into(), "host.docker.internal:host-gateway".into()]);
        }
    }
    a.extend(["-v".into(), format!("{p}:{p}"), "-w".into(), p.clone()]);
    if opts.sandbox {
        // Raw sockets would skip the sandbox VM's egress rules.
        a.extend(["--cap-drop".into(), "NET_RAW".into(), "--security-opt".into(), "no-new-privileges".into()]);
        if project.join(".git").is_dir() {
            // The share is live, and a .git the sandbox could write is code the Mac runs
            // (hooks, core.fsmonitor) on the next `git status`.
            a.extend(["-v".into(), format!("{p}/.git:{p}/.git:ro")]);
        }
    }
    for dir in &plan.local_dirs {
        a.extend(["-v".into(), format!("{}:{p}/{dir}", local_volume(project, dir))]);
    }
    for (vol, path) in &plan.caches {
        a.extend(["-v".into(), format!("{vol}:{path}")]);
    }
    if project.join(".env").is_file() && !opts.sandbox {
        a.extend(["--env-file".into(), format!("{p}/.env")]);
    }
    if tty && let Ok(term) = std::env::var("TERM") {
        a.extend(["-e".into(), format!("TERM={term}")]);
    }
    for e in &opts.env {
        a.extend(["-e".into(), e.clone()]);
    }
    // Secrets: names only on the command line; docker reads values from our env.
    for s in &opts.secrets {
        a.extend(["-e".into(), s.clone()]);
    }
    a.push(plan.image.clone());
    if plan.corepack {
        a.extend([
            "sh".into(),
            "-c".into(),
            "corepack enable >/dev/null 2>&1 || npm install -g --silent pnpm yarn >/dev/null 2>&1; exec \"$@\"".into(),
            "--".into(),
        ]);
    }
    a.extend(cmd.iter().cloned());
    a
}

pub fn run(paths: &Paths, cmd: &[String], opts: Options) -> Result<std::convert::Infallible> {
    if cmd.is_empty() {
        bail!("usage: rekcod x [options] <command> [args...]");
    }
    let project = std::env::current_dir()?.canonicalize()?;
    if !project.starts_with("/Users") && !project.starts_with("/Volumes") && !project.starts_with("/private") {
        bail!("{} isn't shared with the VM (only /Users, /Volumes and /private are)", project.display());
    }
    let plan = match &opts.image {
        Some(image) => Plan { image: image.clone(), local_dirs: vec![], caches: vec![], corepack: false },
        None => plan(&project, cmd).with_context(|| format!("can't tell which runtime `{}` needs here; pass --image <image>", cmd[0]))?,
    };
    for s in &opts.secrets {
        if std::env::var_os(s).is_none() {
            bail!("--secret {s}: not set in this shell's environment");
        }
    }
    let target = if opts.sandbox { sandbox_boot(paths, &project)? } else { main_boot(paths)? };
    let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    let args = docker_args(&project, cmd, &plan, &opts, tty);
    if std::env::var_os("REKCOD_DEBUG").is_some() {
        eprintln!("rekcod: docker {}", args.join(" "));
    }
    let mut docker = crate::docker::any_cli(paths)?;
    docker.env("DOCKER_HOST", target.docker_host()).args(&args);
    if !opts.sandbox {
        return Err(docker.exec()).context("exec docker");
    }
    // The sandbox VM stops itself after SANDBOX_IDLE without a command running, so a killed
    // `rekcod` or `--keep` doesn't leave it up for good.
    let used = target.home.join("run/last-used");
    let _ = std::fs::write(&used, b"");
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _ = std::fs::write(&used, b"");
        }
    });
    let status = docker.status().context("run docker")?;
    if !opts.keep {
        let _ = crate::cli::stop_daemon(&target);
    }
    std::process::exit(status.code().unwrap_or(1));
}

fn main_boot(paths: &Paths) -> Result<Paths> {
    if crate::cli::running_pid(paths).is_none() {
        let t0 = Instant::now();
        eprintln!("rekcod: starting the VM");
        crate::cli::boot(paths, &crate::cli::BootOptions::default())?;
        eprintln!("rekcod: ready in {:.1}s", t0.elapsed().as_secs_f64());
    }
    Ok(paths.clone())
}

/// Seconds the `--sandbox` VM stays up with no `rekcod x` running in it.
const SANDBOX_IDLE: u64 = 30 * 60;

/// The sandbox VM's own home (`~/.rekcod/sandbox`): data disk, MAC, sockets.
pub fn sandbox_paths(paths: &Paths) -> Result<Paths> {
    Paths::at(paths.home.join("sandbox"))
}

/// Boots the sandbox VM for `project`, replacing one running for another directory.
fn sandbox_boot(paths: &Paths, project: &Path) -> Result<Paths> {
    let sb = sandbox_paths(paths)?;
    if crate::cli::running_pid(&sb).is_some() {
        let status: serde_json::Value =
            std::fs::read(sb.status_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if status["project"].as_str().map(PathBuf::from).as_deref() == Some(project) {
            return Ok(sb);
        }
        eprintln!("rekcod: restarting the sandbox VM for {}", project.display());
        crate::cli::stop_daemon(&sb)?;
    }
    let t0 = Instant::now();
    eprintln!("rekcod: starting a sandbox VM that sees only {}", project.display());
    let opts = crate::cli::BootOptions {
        memory_gib: Some(4),
        disk_gib: 64,
        project: Some(project.to_path_buf()),
        guest_dir: Some(paths.guest_dir()?),
        idle_ttl: Some(SANDBOX_IDLE),
        ..Default::default()
    };
    crate::cli::boot(&sb, &opts)?;
    eprintln!("rekcod: sandbox ready in {:.1}s", t0.elapsed().as_secs_f64());
    Ok(sb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rekcod-x-{}-{}",
            std::process::id(),
            files.len() + files.first().map_or(0, |f| f.0.len() * 31 + f.1.len())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    fn image(files: &[(&str, &str)], cmd: &str) -> String {
        let dir = project(files);
        let cmd: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        let p = plan(&dir, &cmd).map(|p| p.image).unwrap_or_default();
        std::fs::remove_dir_all(dir).unwrap();
        p
    }

    #[test]
    fn picks_runtime_and_version() {
        assert_eq!(image(&[(".nvmrc", "v20.11.1\n")], "npm run dev"), "node:20-slim");
        assert_eq!(image(&[("package.json", r#"{"engines":{"node":">=22.0"}}"#)], "./start.sh"), "node:22-slim");
        assert_eq!(image(&[("package.json", "{}")], "node x.js"), "node:lts-slim");
        assert_eq!(image(&[("bun.lock", "")], "bun dev"), "oven/bun:1");
        assert_eq!(image(&[(".python-version", "3.12.4")], "python app.py"), "python:3.12-slim");
        assert_eq!(
            image(&[("pyproject.toml", "requires-python = \">=3.11\"\n"), ("uv.lock", "")], "uv run app"),
            "ghcr.io/astral-sh/uv:python3.11-bookworm-slim"
        );
        assert_eq!(image(&[("go.mod", "module x\n\ngo 1.23.2\n")], "go test ./..."), "golang:1.23");
        assert_eq!(image(&[(".tool-versions", "nodejs 18.19.0\npython 3.11.2\n")], "make"), "node:18-slim");
        assert_eq!(image(&[("mise.toml", "[tools]\npython = \"3.10\"\n")], "pytest"), "python:3.10-slim");
        // The command wins over unrelated project files.
        assert_eq!(image(&[("package.json", "{}")], "python3 tool.py"), "python:3.14-slim");
        assert_eq!(image(&[], "./deploy.sh"), "");
    }

    #[test]
    fn project_files_are_read_without_following_links() {
        let dir = std::env::temp_dir().join(format!("rekcod-x-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let outside = dir.with_extension("outside");
        std::fs::write(&outside, "3.9\n").unwrap();
        std::os::unix::fs::symlink("/dev/zero", dir.join(".nvmrc")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join(".python-version")).unwrap();
        // A link is an error, not a read, and detection goes on without it.
        assert!(read_project_file(&dir, ".nvmrc").unwrap_err().to_string().contains("symlink"));
        assert!(read_project_file(&dir, ".python-version").is_err());
        let cloned = Project { dir: &dir, follow: false };
        assert_eq!(plan_in(&cloned, &["node".into()]).unwrap().image, "node:lts-slim");
        assert_eq!(plan_in(&cloned, &["python".into()]).unwrap().image, "python:3.14-slim");
        assert!(!has_project_file(&dir, ".python-version"));
        // The Mac owner's own project follows links (a monorepo's shared .python-version),
        // but still only to a regular file.
        assert!(users_own(&dir));
        assert_eq!(plan(&dir, &["python".into()]).unwrap().image, "python:3.9-slim");
        assert_eq!(plan(&dir, &["node".into()]).unwrap().image, "node:lts-slim");
        // A FIFO doesn't block the open; a file over the limit isn't read.
        let fifo = std::ffi::CString::new(dir.join("go.mod").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_project_file(&dir, "go.mod").unwrap_err().to_string().contains("regular"));
        assert!(read_file(&dir, "go.mod", true).unwrap_err().to_string().contains("regular"));
        std::fs::write(dir.join("package.json"), vec![b' '; MAX_PROJECT_FILE as usize + 1]).unwrap();
        assert!(read_project_file(&dir, "package.json").unwrap_err().to_string().contains("over"));
        assert_eq!(read_project_file(&dir, "missing").unwrap(), None);
        assert!(read_project_file(&dir, "../outside").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn docker_args_mount_project_and_deps() {
        let project = Path::new("/Users/me/My App");
        let plan = Plan { image: "node:22-slim".into(), local_dirs: vec!["node_modules"], caches: vec![], corepack: true };
        let opts = Options {
            image: None,
            publish: vec![],
            env: vec!["A=1".into()],
            no_autoports: false,
            sandbox: false,
            offline: false,
            secrets: vec!["TOKEN".into()],
            keep: false,
        };
        let a = docker_args(project, &["pnpm".into(), "dev".into()], &plan, &opts, false).join(" ");
        assert!(a.contains("--network host --label dev.rekcod.autoports=1"), "{a}");
        assert!(a.contains("-v /Users/me/My App:/Users/me/My App -w /Users/me/My App"), "{a}");
        assert!(a.contains(&format!("-v {}:/Users/me/My App/node_modules", local_volume(project, "node_modules"))), "{a}");
        assert!(a.contains("-e A=1 -e TOKEN node:22-slim sh -c"), "{a}");
        assert!(a.ends_with("-- pnpm dev"), "{a}");

        let opts = Options { publish: vec!["3000".into()], sandbox: true, ..opts };
        let a = docker_args(project, &["node".into()], &plan, &opts, false).join(" ");
        assert!(a.contains("-p 127.0.0.1:3000:3000") && !a.contains("host-gateway") && !a.contains("autoports"), "{a}");
    }

    #[test]
    fn local_volume_names_are_stable_and_valid() {
        let a = local_volume(Path::new("/Users/me/My App"), ".venv");
        assert_eq!(a, local_volume(Path::new("/Users/me/My App"), ".venv"));
        assert_ne!(a, local_volume(Path::new("/Users/other/My App"), ".venv"));
        assert!(a.starts_with("rekcod-x_my-app_") && a.ends_with("_venv"), "{a}");
    }
}
