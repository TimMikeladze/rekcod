//! launchd jobs for rekcod's long-running commands (`controller serve`, `node serve`,
//! `edge serve`, `storage serve` with `--install`, `start --install`).
//!
//! Each is a LaunchAgent (`~/Library/LaunchAgents`, starts at login) or, on a server, a
//! LaunchDaemon (`/Library/LaunchDaemons`, starts at boot with nobody logged in) that runs
//! as the same user. A name that already has a daemon stays one;
//! `REKCOD_LAUNCHD=system` makes new ones daemons. Daemons need root to install, so this
//! runs `launchctl` and the plist copy through sudo unless it is root.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;

pub fn label(name: &str) -> String {
    format!("dev.rekcod.{name}")
}

fn plist_path(name: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(Path::new(&home).join("Library/LaunchAgents").join(format!("{}.plist", label(name))))
}

pub fn daemon_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/Library/LaunchDaemons/{}.plist", label(name)))
}

pub fn is_daemon(name: &str) -> bool {
    daemon_path(name).exists()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Who a daemon runs as: launchd starts it outside any login, so it needs the user and
/// the environment a login would have given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAs {
    pub user: String,
    pub home: PathBuf,
}

impl RunAs {
    /// This process's user.
    pub fn current() -> Result<RunAs> {
        let pw = unsafe { libc::getpwuid(libc::getuid()) };
        Self::from_passwd(pw).context("no passwd entry for this user")
    }

    pub fn named(user: &str) -> Result<RunAs> {
        let c = std::ffi::CString::new(user)?;
        let pw = unsafe { libc::getpwnam(c.as_ptr()) };
        Self::from_passwd(pw).with_context(|| format!("no user {user}"))
    }

    fn from_passwd(pw: *mut libc::passwd) -> Option<RunAs> {
        if pw.is_null() {
            return None;
        }
        let s = |p: *const libc::c_char| unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().to_string();
        let (user, home) = unsafe { (s((*pw).pw_name), s((*pw).pw_dir)) };
        Some(RunAs { user, home: home.into() })
    }
}

/// A KeepAlive job: `exe args...` with `env`, logging to `log`.
/// `crash_only`: launchd restarts it only after a non-zero exit, so a deliberate stop
/// sticks. `run_as` makes it a daemon for that user.
#[derive(Debug, Clone, Default)]
pub struct Job {
    pub name: String,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub log: PathBuf,
    /// `REKCOD_HOME` (other homes only) and the variables in [`CARRIED`] that were set.
    pub env: Vec<(String, String)>,
    pub crash_only: bool,
    pub run_as: Option<RunAs>,
}

pub fn plist_with(job: &Job) -> String {
    let argv: String = std::iter::once(job.exe.display().to_string())
        .chain(job.args.iter().cloned())
        .map(|a| format!("    <string>{}</string>\n", xml_escape(&a)))
        .collect();
    let mut vars: Vec<(String, String)> =
        job.env.iter().filter(|(k, _)| !matches!(k.as_str(), "HOME" | "USER" | "PATH")).cloned().collect();
    if let Some(r) = &job.run_as {
        vars.push(("HOME".into(), r.home.display().to_string()));
        vars.push(("USER".into(), r.user.clone()));
        vars.push(("PATH".into(), "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()));
    }
    let env = if vars.is_empty() {
        String::new()
    } else {
        let kv: String = vars.iter().map(|(k, v)| format!("<key>{}</key><string>{}</string>", xml_escape(k), xml_escape(v))).collect();
        format!("  <key>EnvironmentVariables</key>\n  <dict>{kv}</dict>\n")
    };
    let user = job
        .run_as
        .as_ref()
        .map(|r| format!("  <key>UserName</key><string>{}</string>\n  <key>GroupName</key><string>staff</string>\n", xml_escape(&r.user)))
        .unwrap_or_default();
    let log = xml_escape(&job.log.display().to_string());
    let keep_alive = if job.crash_only { "<dict><key>SuccessfulExit</key><false/></dict>" } else { "<true/>" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array>
{argv}  </array>
{user}{env}  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>{keep_alive}
  <key>ThrottleInterval</key><integer>5</integer>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label(&job.name)
    )
}

/// Variables that change what a job does, copied from the `--install` run into its plist so
/// the job behaves like the run that was tested. None of them is a secret: plists of
/// daemons are world-readable.
const CARRIED: &[&str] = &[
    "REKCOD_INSECURE",
    "REKCOD_GUEST_DIR",
    "REKCOD_NS",
    "REKCOD_DEBUG",
    "REKCOD_KERNEL_CMDLINE",
    "REKCOD_DISK_SYNC",
    "REKCOD_BALLOON",
    "REKCOD_NO_BALLOON",
    "REKCOD_DOCKER_CLI",
    "REKCOD_DASHBOARD_DIR",
    "REKCOD_SYSTEM_SOCKET",
    "REKCOD_NODE",
    "DOCKER_CONFIG",
    "S3_ENDPOINT",
    "S3_BUCKET",
];

/// Secrets a foreground run may have used; they never go into a plist.
const NOT_CARRIED: &[&str] = &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN"];

/// [`CARRIED`] as `get` has them.
fn carried(get: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    CARRIED.iter().filter_map(|k| get(k).filter(|v| !v.is_empty()).map(|v| (k.to_string(), v))).collect()
}

/// `args` without `flag VALUE` / `flag=VALUE`, and the value.
pub fn take_flag(args: &[String], flag: &str) -> (Vec<String>, Option<String>) {
    let (mut out, mut value) = (Vec::with_capacity(args.len()), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            value = it.next().cloned();
        } else if let Some(v) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            value = Some(v.to_string());
        } else {
            out.push(a.clone());
        }
    }
    (out, value)
}

/// The command line this process was started with, minus `--install`.
pub fn args_without_install() -> Vec<String> {
    strip(&std::env::args().skip(1).collect::<Vec<_>>())
}

pub fn strip(argv: &[String]) -> Vec<String> {
    argv.iter().filter(|a| *a != "--install").cloned().collect()
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn domain_of(name: &str) -> String {
    if is_daemon(name) { "system".into() } else { gui_domain() }
}

fn launchctl(args: &[&str]) -> Result<Output> {
    Ok(Command::new("/bin/launchctl").args(args).output()?)
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// `args` as root: directly when this is root, else through sudo (which asks in a terminal,
/// and otherwise only works with credentials already cached for this terminal).
fn root(args: &[&str]) -> Result<Output> {
    if is_root() {
        return Ok(Command::new(args[0]).args(&args[1..]).output()?);
    }
    let mut sudo = Command::new("/usr/bin/sudo");
    if !std::io::stdin().is_terminal() {
        sudo.arg("-n");
    }
    let out = sudo.args(args).stdin(Stdio::inherit()).output()?;
    if !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("password is required") {
        bail!("changing a LaunchDaemon needs sudo; run this in a terminal (`sudo -v` first)");
    }
    Ok(out)
}

/// Whether daemons can be installed from here: root, or sudo that works now (asking for
/// the password when there's a terminal).
pub fn can_install_daemons() -> bool {
    if is_root() {
        return true;
    }
    let mut sudo = Command::new("/usr/bin/sudo");
    sudo.arg("-v");
    if !std::io::stdin().is_terminal() {
        sudo.arg("-n");
    }
    sudo.stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Writes the job and loads it now. `REKCOD_NO_LAUNCHCTL` writes it into the rekcod home
/// instead and loads nothing (dry run).
pub fn install(paths: &Paths, name: &str, args: &[String]) -> Result<()> {
    install_with(paths, name, args, false)
}

pub fn installed(name: &str) -> bool {
    plist_path(name).is_ok_and(|p| p.exists()) || is_daemon(name)
}

/// The job's plist: the daemon's when there is one, else the agent's.
fn job_plist(name: &str) -> Option<PathBuf> {
    if is_daemon(name) {
        return Some(daemon_path(name));
    }
    plist_path(name).ok().filter(|p| p.exists())
}

/// The `REKCOD_HOME` a plist runs its job with; none means the default home.
fn recorded_home(plist: &Path) -> Option<PathBuf> {
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", "EnvironmentVariables.REKCOD_HOME", "raw", "-o", "-"])
        .arg(plist)
        .output()
        .ok()?;
    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !h.is_empty()).then(|| PathBuf::from(h))
}

/// Whether a job recorded with `recorded` (its plist's `REKCOD_HOME`) is this home's.
/// Labels carry no home, so another home's `--install` or `uninstall` must not touch it.
fn owns(paths: &Paths, recorded: Option<&Path>) -> bool {
    match recorded {
        Some(h) => h == paths.home,
        None => paths.is_default_home(),
    }
}

/// Whether job `name` is installed for this rekcod home.
pub fn installed_here(paths: &Paths, name: &str) -> bool {
    job_plist(name).is_some_and(|p| owns(paths, recorded_home(&p).as_deref()))
}

/// The home a job installed for another rekcod home runs with, if it is one.
fn other_home(paths: &Paths, name: &str) -> Option<String> {
    let p = job_plist(name)?;
    let recorded = recorded_home(&p);
    (!owns(paths, recorded.as_deref())).then(|| recorded.map_or("~/.rekcod".into(), |h| h.display().to_string()))
}

/// Fleet services in the order `upgrade` restarts them. The controller goes first: it
/// accepts nodes and edges one minor version behind it, never ahead.
pub const FLEET_SERVICES: &[&str] = &["controller", "node", "edge", "storage"];

/// The installed job's command line (the binary first), read back from its plist.
pub fn job_args(name: &str) -> Option<Vec<String>> {
    let path = if is_daemon(name) { daemon_path(name) } else { plist_path(name).ok().filter(|p| p.exists())? };
    agent_job(name, &path).ok().map(|j| std::iter::once(j.exe.display().to_string()).chain(j.args).collect())
}

/// Stops the job and starts it again from its plist (`launchctl kickstart -k`), so it
/// runs whatever binary is at its path now.
pub fn restart(name: &str) -> Result<()> {
    let target = format!("{}/{}", domain_of(name), label(name));
    let out =
        if is_daemon(name) { root(&["/bin/launchctl", "kickstart", "-k", &target])? } else { launchctl(&["kickstart", "-k", &target])? };
    if !out.status.success() {
        bail!("launchctl kickstart -k {target}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Starts the job now if it isn't running (no-op otherwise).
pub fn kickstart(name: &str) {
    let target = format!("{}/{}", domain_of(name), label(name));
    let _ = if is_daemon(name) { root(&["/bin/launchctl", "kickstart", &target]) } else { launchctl(&["kickstart", &target]) };
}

/// The path to put in a job, a firewall exception or anything else that outlives this
/// binary: for a Homebrew keg (`<prefix>/Cellar/rekcod/<version>/bin/rekcod`, gone at the
/// next `brew upgrade`) the `<prefix>/bin/rekcod` link to it, else the binary's own path.
pub fn stable_exe() -> Result<PathBuf> {
    Ok(stable_path(&std::env::current_exe()?.canonicalize()?))
}

fn stable_path(exe: &Path) -> PathBuf {
    let parts: Vec<_> = exe.components().collect();
    let Some(i) = parts.iter().position(|c| c.as_os_str() == "Cellar") else { return exe.to_path_buf() };
    let link: PathBuf = parts[..i].iter().collect::<PathBuf>().join("bin").join(exe.file_name().unwrap_or_default());
    if link.canonicalize().is_ok_and(|t| t == exe) { link } else { exe.to_path_buf() }
}

fn wants_daemon(name: &str) -> bool {
    is_daemon(name) || std::env::var("REKCOD_LAUNCHD").is_ok_and(|v| v == "system")
}

/// Flags whose value is a path. launchd starts jobs in `/`, so a relative one is made
/// absolute against where `--install` ran (not by a WorkingDirectory: that directory may
/// be gone by the next boot, the pkg's scripts directory always is).
const PATH_FLAGS: &[&str] = &["--db", "--data", "--backup-dir", "--storage-credentials"];

fn absolute_paths(args: &[String], cwd: &Path) -> Vec<String> {
    let abs = |v: &str| if Path::new(v).is_absolute() { v.to_string() } else { cwd.join(v).display().to_string() };
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if PATH_FLAGS.contains(&a.as_str()) {
            out.push(a.clone());
            if let Some(v) = it.next() {
                out.push(abs(v));
            }
        } else if let Some((flag, v)) = a.split_once('=').filter(|(f, _)| PATH_FLAGS.contains(f)) {
            out.push(format!("{flag}={}", abs(v)));
        } else {
            out.push(a.clone());
        }
    }
    out
}

/// The job `--install` writes for `name`: this binary by its stable path, its path
/// arguments absolute, with the environment that changes what it does.
fn job_for(paths: &Paths, name: &str, args: &[String], crash_only: bool) -> Result<Job> {
    let log = paths.home.join("logs").join(format!("{name}.log"));
    let mut env = Vec::new();
    if !paths.is_default_home() {
        env.push(("REKCOD_HOME".to_string(), paths.home.display().to_string()));
    }
    env.extend(carried(|k| std::env::var(k).ok()));
    let left: Vec<&str> = NOT_CARRIED.iter().copied().filter(|k| std::env::var_os(k).is_some()).collect();
    if !left.is_empty() {
        eprintln!("rekcod: {} not written into the {name} job (secrets don't go into plists)", left.join(", "));
    }
    Ok(Job {
        name: name.into(),
        exe: stable_exe()?,
        args: absolute_paths(args, &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
        log,
        env,
        crash_only,
        run_as: None,
    })
}

pub fn install_with(paths: &Paths, name: &str, args: &[String], crash_only: bool) -> Result<()> {
    let mut job = job_for(paths, name, args, crash_only)?;
    std::fs::create_dir_all(job.log.parent().context("log dir")?)?;
    let dry = std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some();
    if !dry && let Some(other) = other_home(paths, name) {
        bail!("{} is installed for the rekcod home {other}; remove it there first (`rekcod uninstall` with that REKCOD_HOME)", label(name));
    }
    let restarts = if crash_only { "restarts if it crashes" } else { "restarts if it exits" };
    if dry {
        let path = paths.home.join(format!("{}.plist", label(name)));
        std::fs::write(&path, plist_with(&job))?;
        println!("wrote {} (dry run: not loaded)", path.display());
        return Ok(());
    }
    if wants_daemon(name) {
        let run_as = RunAs::current()?;
        job.run_as = Some(run_as.clone());
        install_daemon(name, &plist_with(&job))?;
        remove_agent(name)?;
        println!("{name} installed: runs now and at boot as {}, {restarts}; log {}", run_as.user, job.log.display());
    } else {
        let path = plist_path(name)?;
        std::fs::create_dir_all(path.parent().context("plist dir")?)?;
        std::fs::write(&path, plist_with(&job))?;
        load(&gui_domain(), name, &path, false)?;
        println!("{name} installed: runs now and at login, {restarts}; log {}", job.log.display());
    }
    report_early_exit(name, &job.log);
    Ok(())
}

/// A job that exits at once (a bad path, a port in use) is restarted every few seconds
/// with nothing said: look once, after it had time to fail.
fn report_early_exit(name: &str, log: &Path) {
    std::thread::sleep(std::time::Duration::from_secs(2));
    let target = format!("{}/{}", domain_of(name), label(name));
    let Ok(out) = launchctl(&["print", &target]) else { return };
    let text = String::from_utf8_lossy(&out.stdout);
    let running = text.lines().any(|l| l.trim_start().starts_with("pid = "));
    let last = text.lines().find_map(|l| l.trim().strip_prefix("last exit code = ")).unwrap_or("");
    if out.status.success() && !running && !last.is_empty() && last != "0" {
        eprintln!("rekcod: {name} exited right away ({last}); see {}", log.display());
    }
}

/// Replaces a previous registration (new arguments, or the binary moved). bootout returns
/// before the old job is gone, and bootstrap fails with "5: Input/output error" until it
/// is, so wait for it and retry.
fn load(domain: &str, name: &str, path: &Path, as_root: bool) -> Result<()> {
    let ctl = |args: &[&str]| -> Result<Output> {
        let mut a = vec!["/bin/launchctl"];
        a.extend_from_slice(args);
        if as_root { root(&a) } else { launchctl(args) }
    };
    let target = format!("{domain}/{}", label(name));
    let _ = ctl(&["bootout", &target]);
    for _ in 0..50 {
        if !ctl(&["print", &target]).is_ok_and(|o| o.status.success()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let p = path.display().to_string();
    let mut out = ctl(&["bootstrap", domain, &p])?;
    for _ in 0..10 {
        if out.status.success() || !String::from_utf8_lossy(&out.stderr).contains("Input/output error") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        out = ctl(&["bootstrap", domain, &p])?;
    }
    if !out.status.success() {
        bail!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Copies `plist` to /Library/LaunchDaemons (root:wheel 0644, which launchd insists on)
/// and loads it into the system domain.
fn install_daemon(name: &str, plist: &str) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("{}-{}.plist", label(name), std::process::id()));
    std::fs::write(&tmp, plist)?;
    let dest = daemon_path(name);
    let out =
        root(&["/usr/bin/install", "-m", "644", "-o", "root", "-g", "wheel", &tmp.display().to_string(), &dest.display().to_string()]);
    let _ = std::fs::remove_file(&tmp);
    let out = out?;
    if !out.status.success() {
        bail!("writing {}: {}", dest.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    load("system", name, &dest, true)
}

/// The LaunchAgent, if any: unloaded and deleted (a daemon took over, or uninstall).
fn remove_agent(name: &str) -> Result<bool> {
    let path = plist_path(name)?;
    let _ = launchctl(&["bootout", &format!("{}/{}", gui_domain(), label(name))]);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub fn uninstall(name: &str) -> Result<()> {
    let mut removed = remove_agent(name)?;
    if is_daemon(name) {
        let _ = root(&["/bin/launchctl", "bootout", &format!("system/{}", label(name))]);
        let out = root(&["/bin/rm", "-f", &daemon_path(name).display().to_string()])?;
        if !out.status.success() {
            bail!("removing {}: {}", daemon_path(name).display(), String::from_utf8_lossy(&out.stderr).trim());
        }
        removed = true;
    }
    if removed {
        println!("{name} removed");
    } else {
        println!("{name} was not installed");
    }
    Ok(())
}

/// An agent's job read back from its plist (`plutil` turns it into JSON): everything a
/// daemon for it needs.
fn agent_job(name: &str, path: &Path) -> Result<Job> {
    let out = Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-"]).arg(path).output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| format!("read {}", path.display()))?;
    let argv: Vec<String> =
        v["ProgramArguments"].as_array().context("no ProgramArguments")?.iter().filter_map(|a| a.as_str().map(String::from)).collect();
    let (exe, args) = argv.split_first().context("empty ProgramArguments")?;
    let env = v["EnvironmentVariables"]
        .as_object()
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
        .unwrap_or_default();
    Ok(Job {
        name: name.into(),
        exe: exe.into(),
        args: args.to_vec(),
        log: v["StandardOutPath"].as_str().unwrap_or_default().into(),
        env,
        crash_only: v["KeepAlive"].is_object(),
        run_as: None,
    })
}

/// `rekcod server at-boot`: the user's LaunchAgents among `names` become LaunchDaemons
/// that run as that user, same command line, same log.
pub fn agents_to_daemons(run_as: &RunAs, names: &[&str]) -> Result<Vec<String>> {
    let mut moved = Vec::new();
    for name in names {
        let agent = run_as.home.join("Library/LaunchAgents").join(format!("{}.plist", label(name)));
        if !agent.exists() {
            continue;
        }
        let mut job = agent_job(name, &agent)?;
        job.run_as = Some(run_as.clone());
        install_daemon(name, &plist_with(&job))?;
        // The agent's own domain is the user's: root unloads it there by uid.
        let uid = std::fs::metadata(&run_as.home).map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or(0);
        let _ = Command::new("/bin/launchctl").args(["bootout", &format!("gui/{uid}/{}", label(name))]).output();
        std::fs::remove_file(&agent)?;
        moved.push(name.to_string());
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_install() {
        let a: Vec<String> = ["controller", "serve", "--install", "--db", "/x/fleet.db"].map(String::from).into();
        assert_eq!(strip(&a), ["controller", "serve", "--db", "/x/fleet.db"]);
    }

    fn job(name: &str, exe: &str, args: &[&str]) -> Job {
        Job {
            name: name.into(),
            exe: exe.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            log: "/h/logs/x.log".into(),
            ..Default::default()
        }
    }

    #[test]
    fn plist_keeps_alive_and_logs() {
        let mut j = job("edge", "/opt/r&d/rekcod", &["edge", "serve", "--data", "./rustfs"]);
        j.env = vec![("REKCOD_HOME".into(), "/h".into()), ("REKCOD_INSECURE".into(), "1".into())];
        let p = plist_with(&j);
        assert!(p.contains("<string>/opt/r&amp;d/rekcod</string>\n    <string>edge</string>\n    <string>serve</string>"), "{p}");
        assert!(p.contains("<key>KeepAlive</key><true/>") && p.contains("<string>dev.rekcod.edge</string>"));
        assert!(p.contains("<key>REKCOD_HOME</key><string>/h</string><key>REKCOD_INSECURE</key><string>1</string>"));
        // plutil is the authority on whether launchd can read it.
        let f = std::env::temp_dir().join(format!("rekcod-plist-{}.plist", std::process::id()));
        std::fs::write(&f, &p).unwrap();
        let ok = Command::new("/usr/bin/plutil").arg("-lint").arg(&f).output().map(|o| o.status.success()).unwrap_or(true);
        // at-boot rebuilds a daemon from the agent: nothing of the job is lost on the way.
        let back = agent_job("edge", &f).unwrap();
        std::fs::remove_file(&f).unwrap();
        assert!(ok, "plutil rejected:\n{p}");
        assert_eq!((back.exe, back.args, back.log), (j.exe, j.args, j.log));
        let mut env = back.env;
        env.sort();
        assert_eq!(env, j.env);
        let mut vm = job("vm", "/r", &["start"]);
        vm.crash_only = true;
        assert!(plist_with(&vm).contains("<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>"));
    }

    /// A relative --db or --data in the job means what it meant where --install ran.
    #[test]
    fn relative_paths_become_absolute() {
        let a: Vec<String> =
            ["controller", "serve", "--db", "fleet.db", "--backup-dir=../b", "--storage-credentials", "/abs/c.json", "--domain", "x"]
                .map(String::from)
                .into();
        assert_eq!(
            absolute_paths(&a, Path::new("/srv")),
            [
                "controller",
                "serve",
                "--db",
                "/srv/fleet.db",
                "--backup-dir=/srv/../b",
                "--storage-credentials",
                "/abs/c.json",
                "--domain",
                "x"
            ]
        );
        let s: Vec<String> = ["storage", "serve", "--data", "./rustfs"].map(String::from).into();
        assert_eq!(absolute_paths(&s, Path::new("/h"))[3], "/h/./rustfs");
    }

    #[test]
    fn only_listed_variables_reach_a_plist() {
        let env = |k: &str| matches!(k, "REKCOD_NS" | "AWS_SECRET_ACCESS_KEY" | "GITHUB_TOKEN" | "S3_BUCKET").then(|| "v".to_string());
        assert_eq!(carried(env), [("REKCOD_NS".to_string(), "v".to_string()), ("S3_BUCKET".into(), "v".into())]);
        assert!(NOT_CARRIED.iter().all(|k| !CARRIED.contains(k)));
    }

    #[test]
    fn flag_taken_out_of_args() {
        let a: Vec<String> = ["controller", "serve", "--alert-webhook", "https://hooks/x", "--auto-approve"].map(String::from).into();
        assert_eq!(
            take_flag(&a, "--alert-webhook"),
            (vec!["controller".into(), "serve".into(), "--auto-approve".into()], Some("https://hooks/x".into()))
        );
        let b: Vec<String> = ["serve", "--alert-webhook=https://h"].map(String::from).into();
        assert_eq!(take_flag(&b, "--alert-webhook"), (vec!["serve".to_string()], Some("https://h".into())));
    }

    /// A Homebrew keg's path becomes the prefix's bin link to it, whatever ran it.
    #[test]
    fn cellar_path_becomes_the_bin_link() {
        let t = std::env::temp_dir().join(format!("rekcod-brew-{}", std::process::id()));
        let keg = t.join("Cellar/rekcod/1.0/bin");
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::create_dir_all(t.join("bin")).unwrap();
        std::fs::write(keg.join("rekcod"), "").unwrap();
        let t = t.canonicalize().unwrap();
        let exe = t.join("Cellar/rekcod/1.0/bin/rekcod");
        assert_eq!(stable_path(&exe), exe, "no link yet");
        std::os::unix::fs::symlink("../Cellar/rekcod/1.0/bin/rekcod", t.join("bin/rekcod")).unwrap();
        assert_eq!(stable_path(&exe), t.join("bin/rekcod"));
        assert_eq!(stable_path(Path::new("/usr/local/bin/rekcod")), Path::new("/usr/local/bin/rekcod"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// A job is this home's when its plist names this home, or names none and this is
    /// ~/.rekcod.
    #[test]
    fn jobs_belong_to_the_home_in_their_plist() {
        let dir = std::env::temp_dir().join(format!("rekcod-owns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let default = Paths { home: PathBuf::from(std::env::var_os("HOME").unwrap()).join(".rekcod") };
        let other = Paths { home: dir.join("h") };
        let plist = |home: Option<&Path>| {
            let f = dir.join(format!("{}.plist", home.is_some()));
            let mut j = job("vm", "/r", &["start"]);
            j.env = home.map(|h| vec![("REKCOD_HOME".to_string(), h.display().to_string())]).unwrap_or_default();
            std::fs::write(&f, plist_with(&j)).unwrap();
            recorded_home(&f)
        };
        let none = plist(None);
        assert_eq!(none, None);
        assert!(owns(&default, none.as_deref()) && !owns(&other, none.as_deref()));
        let h = plist(Some(&other.home));
        assert_eq!(h.as_deref(), Some(other.home.as_path()));
        assert!(owns(&other, h.as_deref()) && !owns(&default, h.as_deref()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A daemon names its user and gets the environment a login would have set.
    #[test]
    fn daemon_plist_runs_as_user() {
        let r = RunAs { user: "me".into(), home: "/Users/me".into() };
        let mut j = job("node", "/Users/me/.rekcod/bin/rekcod", &["node", "serve"]);
        j.run_as = Some(r);
        let p = plist_with(&j);
        assert!(p.contains("<key>UserName</key><string>me</string>") && p.contains("<key>GroupName</key><string>staff</string>"));
        assert!(p.contains("<key>HOME</key><string>/Users/me</string>") && p.contains("<key>USER</key><string>me</string>"));
        let f = std::env::temp_dir().join(format!("rekcod-daemon-{}.plist", std::process::id()));
        std::fs::write(&f, &p).unwrap();
        let back = agent_job("node", &f).unwrap();
        std::fs::remove_file(&f).unwrap();
        assert_eq!(
            (back.exe.display().to_string(), back.args.clone()),
            ("/Users/me/.rekcod/bin/rekcod".into(), vec!["node".to_string(), "serve".into()])
        );
        assert!(!back.crash_only);
        // Converting again doesn't stack a second HOME/USER/PATH.
        let mut again = back;
        again.run_as = j.run_as.clone();
        assert_eq!(plist_with(&again).matches("<key>HOME</key>").count(), 1);
        assert_eq!(RunAs::current().unwrap().user, std::env::var("USER").unwrap_or_default());
    }
}
