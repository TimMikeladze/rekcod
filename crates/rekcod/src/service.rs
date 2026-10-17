//! launchd jobs for rekcod's long-running commands (`controller serve`, `node serve`,
//! `edge serve`, `storage serve` with `--install`, `start --install`).
//!
//! Each is a LaunchAgent (`~/Library/LaunchAgents`, starts at login) or, on a server, a
//! LaunchDaemon (`/Library/LaunchDaemons`, starts at boot with nobody logged in) that runs
//! as the same user (docs/server.md). A name that already has a daemon stays one;
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

/// A KeepAlive job running `exe args...`, logging to `log`. `crash_only`: launchd
/// restarts it only after a non-zero exit, so a deliberate stop sticks. `run_as` makes it a
/// daemon for that user.
pub fn plist_with(
    name: &str,
    exe: &Path,
    args: &[String],
    log: &Path,
    rekcod_home: Option<&Path>,
    crash_only: bool,
    run_as: Option<&RunAs>,
) -> String {
    let argv: String = std::iter::once(exe.display().to_string())
        .chain(args.iter().cloned())
        .map(|a| format!("    <string>{}</string>\n", xml_escape(&a)))
        .collect();
    let mut vars = Vec::new();
    if let Some(h) = rekcod_home {
        vars.push(("REKCOD_HOME", h.display().to_string()));
    }
    if let Some(r) = run_as {
        vars.push(("HOME", r.home.display().to_string()));
        vars.push(("USER", r.user.clone()));
        vars.push(("PATH", "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()));
    }
    let env = if vars.is_empty() {
        String::new()
    } else {
        let kv: String = vars.iter().map(|(k, v)| format!("<key>{k}</key><string>{}</string>", xml_escape(v))).collect();
        format!("  <key>EnvironmentVariables</key>\n  <dict>{kv}</dict>\n")
    };
    let user = run_as
        .map(|r| format!("  <key>UserName</key><string>{}</string>\n  <key>GroupName</key><string>staff</string>\n", xml_escape(&r.user)))
        .unwrap_or_default();
    let log = xml_escape(&log.display().to_string());
    let keep_alive = if crash_only { "<dict><key>SuccessfulExit</key><false/></dict>" } else { "<true/>" };
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
        label(name)
    )
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
    let mut sudo = Command::new("sudo");
    if !std::io::stdin().is_terminal() {
        sudo.arg("-n");
    }
    let out = sudo.args(args).stdin(Stdio::inherit()).output()?;
    if !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("password is required") {
        bail!("installing a LaunchDaemon needs sudo; run this in a terminal (`sudo -v` first)");
    }
    Ok(out)
}

/// Whether daemons can be installed from here: root, or sudo that works now (asking for
/// the password when there's a terminal).
pub fn can_install_daemons() -> bool {
    if is_root() {
        return true;
    }
    let mut sudo = Command::new("sudo");
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

/// Starts the job now if it isn't running (no-op otherwise).
pub fn kickstart(name: &str) {
    let target = format!("{}/{}", domain_of(name), label(name));
    let _ = if is_daemon(name) { root(&["/bin/launchctl", "kickstart", &target]) } else { launchctl(&["kickstart", &target]) };
}

/// The path to put in a job: the one this binary was run by when that's a symlink
/// into a Homebrew Cellar (`/opt/homebrew/bin/rekcod`), whose target changes with every
/// `brew upgrade`; otherwise the binary's own path.
fn stable_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let argv0 = std::env::args().next().unwrap_or_default();
    let invoked = if argv0.contains('/') {
        Some(PathBuf::from(&argv0))
    } else {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(&argv0)).find(|c| c.exists()))
    };
    if let Some(link) = invoked.filter(|l| l.is_absolute() && std::fs::symlink_metadata(l).is_ok_and(|m| m.file_type().is_symlink()))
        && link.canonicalize().is_ok_and(|t| t == exe)
        && exe.components().any(|c| c.as_os_str() == "Cellar")
    {
        return Ok(link);
    }
    Ok(exe)
}

fn wants_daemon(name: &str) -> bool {
    is_daemon(name) || std::env::var("REKCOD_LAUNCHD").is_ok_and(|v| v == "system")
}

pub fn install_with(paths: &Paths, name: &str, args: &[String], crash_only: bool) -> Result<()> {
    let exe = stable_exe()?;
    let log = paths.home.join("logs").join(format!("{name}.log"));
    std::fs::create_dir_all(log.parent().context("log dir")?)?;
    let home = (!paths.is_default_home()).then_some(paths.home.as_path());
    if std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some() {
        let path = paths.home.join(format!("{}.plist", label(name)));
        std::fs::write(&path, plist_with(name, &exe, args, &log, home, crash_only, None))?;
        println!("wrote {} (dry run: not loaded)", path.display());
        return Ok(());
    }
    if wants_daemon(name) {
        let run_as = RunAs::current()?;
        install_daemon(name, &plist_with(name, &exe, args, &log, home, crash_only, Some(&run_as)))?;
        remove_agent(name)?;
        let restarts = if crash_only { "restarts if it crashes" } else { "restarts if it exits" };
        println!("{name} installed: runs now and at boot as {}, {restarts}; log {}", run_as.user, log.display());
        return Ok(());
    }
    let path = plist_path(name)?;
    std::fs::create_dir_all(path.parent().context("plist dir")?)?;
    std::fs::write(&path, plist_with(name, &exe, args, &log, home, crash_only, None))?;
    load(&gui_domain(), name, &path, false)?;
    let restarts = if crash_only { "restarts if it crashes" } else { "restarts if it exits" };
    println!("{name} installed: runs now and at login, {restarts}; log {}", log.display());
    Ok(())
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

/// The agent's `ProgramArguments` and whether it only restarts after a crash, read back
/// from its plist (`plutil` turns it into JSON).
fn agent_job(path: &Path) -> Result<(Vec<String>, bool)> {
    let out = Command::new("plutil").args(["-convert", "json", "-o", "-"]).arg(path).output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| format!("read {}", path.display()))?;
    let argv: Vec<String> =
        v["ProgramArguments"].as_array().context("no ProgramArguments")?.iter().filter_map(|a| a.as_str().map(String::from)).collect();
    Ok((argv, v["KeepAlive"].is_object()))
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
        let (argv, crash_only) = agent_job(&agent)?;
        let (exe, args) = argv.split_first().context("empty ProgramArguments")?;
        let out = Command::new("plutil").args(["-extract", "StandardOutPath", "raw", "-o", "-"]).arg(&agent).output()?;
        let log = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        let out = Command::new("plutil").args(["-extract", "EnvironmentVariables.REKCOD_HOME", "raw", "-o", "-"]).arg(&agent).output()?;
        let home = out.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string()));
        install_daemon(name, &plist_with(name, Path::new(exe), args, &log, home.as_deref(), crash_only, Some(run_as)))?;
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

    #[test]
    fn plist_keeps_alive_and_logs() {
        let p = plist_with(
            "edge",
            Path::new("/opt/r&d/rekcod"),
            &["edge".into(), "serve".into()],
            Path::new("/h/logs/edge.log"),
            Some(Path::new("/h")),
            false,
            None,
        );
        assert!(p.contains("<string>/opt/r&amp;d/rekcod</string>\n    <string>edge</string>\n    <string>serve</string>"), "{p}");
        assert!(p.contains("<key>KeepAlive</key><true/>") && p.contains("<string>dev.rekcod.edge</string>"));
        assert!(p.contains("<key>REKCOD_HOME</key><string>/h</string>"));
        // plutil is the authority on whether launchd can read it.
        let f = std::env::temp_dir().join(format!("rekcod-plist-{}.plist", std::process::id()));
        std::fs::write(&f, &p).unwrap();
        let ok = Command::new("plutil").arg("-lint").arg(&f).output().map(|o| o.status.success()).unwrap_or(true);
        std::fs::remove_file(&f).unwrap();
        assert!(ok, "plutil rejected:\n{p}");
        let vm = plist_with("vm", Path::new("/r"), &["start".into()], Path::new("/l"), None, true, None);
        assert!(vm.contains("<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>"), "{vm}");
    }

    /// A daemon names its user and gets the environment a login would have set.
    #[test]
    fn daemon_plist_runs_as_user() {
        let r = RunAs { user: "tim".into(), home: "/Users/tim".into() };
        let p = plist_with(
            "node",
            Path::new("/Users/tim/.rekcod/bin/rekcod"),
            &["node".into(), "serve".into()],
            Path::new("/l"),
            None,
            false,
            Some(&r),
        );
        assert!(p.contains("<key>UserName</key><string>tim</string>") && p.contains("<key>GroupName</key><string>staff</string>"));
        assert!(p.contains("<key>HOME</key><string>/Users/tim</string>") && p.contains("<key>USER</key><string>tim</string>"));
        let f = std::env::temp_dir().join(format!("rekcod-daemon-{}.plist", std::process::id()));
        std::fs::write(&f, &p).unwrap();
        let (argv, crash_only) = agent_job(&f).unwrap();
        std::fs::remove_file(&f).unwrap();
        assert_eq!(argv, ["/Users/tim/.rekcod/bin/rekcod", "node", "serve"]);
        assert!(!crash_only);
        assert_eq!(RunAs::current().unwrap().user, std::env::var("USER").unwrap_or_default());
    }
}
