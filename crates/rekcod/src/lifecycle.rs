//! Upgrading, removing and diagnosing rekcod (docs/install-upgrade.md).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::paths::Paths;

const REPO: &str = "TimMikeladze/rekcod";
const TARBALL: &str = "rekcod-darwin-arm64.tar.gz";

/// `major.minor.patch` as numbers; anything else sorts first.
pub fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Versions that may talk to each other: same major, and same minor while major is 0.
pub fn compatible(a: &str, b: &str) -> bool {
    let (a, b) = (parse_version(a), parse_version(b));
    a.0 == b.0 && (a.0 > 0 || a.1 == b.1)
}

fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("curl").args(["-fsSL", "--max-time", "60"]).args(args).output().context("run curl")?;
    if !out.status.success() {
        bail!("curl {}: {}", args.last().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn latest_version() -> Result<String> {
    let body = curl(&["-H", "Accept: application/vnd.github+json", &format!("https://api.github.com/repos/{REPO}/releases/latest")])?;
    let v: serde_json::Value = serde_json::from_slice(&body)?;
    v["tag_name"].as_str().map(|t| t.trim_start_matches('v').to_string()).ok_or_else(|| anyhow!("no tag in GitHub's latest release"))
}

fn release_base(version: &str) -> String {
    match std::env::var("REKCOD_RELEASE_URL") {
        Ok(b) => b.trim_end_matches('/').to_string(),
        Err(_) => format!("https://github.com/{REPO}/releases/download/v{version}"),
    }
}

/// `rekcod upgrade [--check]`.
pub fn upgrade(check: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = latest_version()?;
    if parse_version(&latest) <= parse_version(current) {
        println!("rekcod {current} is the latest");
        return Ok(());
    }
    if check {
        println!("rekcod {latest} is out (this is {current}); `rekcod upgrade` installs it");
        std::process::exit(1);
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe.components().any(|c| c.as_os_str() == "Cellar") {
        bail!("this rekcod came from Homebrew; `brew upgrade rekcod`");
    }
    let dir = exe.parent().context("binary has no directory")?;
    let probe = dir.join(".rekcod-upgrade-probe");
    if std::fs::write(&probe, b"").is_err() {
        bail!("can't write to {}; run `sudo rekcod upgrade`", dir.display());
    }
    let _ = std::fs::remove_file(&probe);
    let work = std::env::temp_dir().join(format!("rekcod-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("x"))?;
    let r = (|| -> Result<()> {
        let base = release_base(&latest);
        eprintln!("rekcod: downloading {latest}");
        let tar = work.join(TARBALL);
        std::fs::write(&tar, curl(&[&format!("{base}/{TARBALL}")])?)?;
        let sums = String::from_utf8(curl(&[&format!("{base}/SHA256SUMS")])?)?;
        let want =
            sums.lines().find(|l| l.ends_with(TARBALL)).and_then(|l| l.split_whitespace().next()).context("SHA256SUMS lists no tarball")?;
        let got =
            crate::fetch::parse_shasum(&String::from_utf8_lossy(&Command::new("shasum").args(["-a", "256"]).arg(&tar).output()?.stdout))
                .map(str::to_string)
                .unwrap_or_default();
        if !got.eq_ignore_ascii_case(want) {
            bail!("checksum mismatch: SHA256SUMS says {want}, the download is {got}; not installing it");
        }
        if !Command::new("tar").arg("-xzf").arg(&tar).arg("-C").arg(work.join("x")).status()?.success() {
            bail!("couldn't unpack {TARBALL}");
        }
        // Next to the old one, then renamed over it: a VM daemon running from the old file
        // keeps its inode.
        let new = dir.join(".rekcod.new");
        std::fs::copy(work.join("x/bin/rekcod"), &new)?;
        std::fs::rename(&new, &exe)?;
        let dash = work.join("x/share/rekcod/dashboard");
        let installed = dir.join("../share/rekcod/dashboard");
        if dash.is_dir() && installed.parent().is_some_and(|p| p.is_dir()) {
            let old = installed.with_extension("old");
            let _ = std::fs::remove_dir_all(&old);
            if installed.exists() {
                std::fs::rename(&installed, &old)?;
            }
            Command::new("cp").arg("-R").arg(&dash).arg(&installed).status()?;
            let _ = std::fs::remove_dir_all(&old);
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    r?;
    let _ = Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&exe).stderr(Stdio::null()).status();
    println!("rekcod {latest} installed at {}. A running VM keeps {current} until `rekcod stop && rekcod start`", exe.display());
    Ok(())
}

/// The LaunchAgents rekcod may have installed.
pub const AGENTS: &[&str] = &["vm", "menubar", "dashboard", "controller", "node", "edge", "storage"];

/// `rekcod uninstall [--keep-data] [--yes]`.
pub fn uninstall(paths: &Paths, keep_data: bool, yes: bool) -> Result<()> {
    let what = if keep_data {
        "stop rekcod, remove its login agents, hand back /var/run/docker.sock and the docker context"
    } else {
        "do that and also delete ~/.rekcod: VM disks, volumes, images, snapshots, sandboxes"
    };
    if !yes {
        eprint!("This will {what}. Type `yes` to go on: ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim() != "yes" {
            bail!("nothing changed");
        }
    }
    let step = |name: &str, r: Result<()>| match r {
        Ok(()) => println!("ok    {name}"),
        Err(e) => println!("skip  {name}: {e:#}"),
    };
    step("stop agent sandboxes", crate::sandbox::rm(paths, &[], true));
    step("stop the VM", crate::cli::stop(paths, true, false));
    for a in AGENTS {
        if crate::service::installed(a) {
            step(&format!("remove {}", crate::service::label(a)), crate::service::uninstall(a));
        }
    }
    if crate::syssock::installed(paths) {
        step("hand back /var/run/docker.sock", crate::syssock::uninstall(paths));
    }
    if crate::docker::available() {
        let prev = std::fs::read_to_string(paths.prev_context()).unwrap_or_default().trim().to_string();
        let back = if !prev.is_empty() && crate::docker::context_exists(&prev) { prev } else { "default".into() };
        if crate::docker::current_context().is_ok_and(|c| c == crate::docker::CONTEXT) {
            step(&format!("switch docker context to {back}"), crate::docker::use_context(&back));
        }
        if crate::docker::context_exists(crate::docker::CONTEXT) {
            step("remove docker context rekcod", crate::docker::run_ok(&["context", "rm", "-f", crate::docker::CONTEXT]).map(|_| ()));
        }
    }
    if !keep_data {
        step("remove rekcod's Include from ~/.ssh/config", crate::sshkey::remove_include());
        step(&format!("delete {}", paths.home.display()), std::fs::remove_dir_all(&paths.home).map_err(Into::into));
    }
    let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "rekcod".into());
    println!("\nThe binary is still at {exe}; delete it with `rm {exe}` (sudo if it's in /usr/local/bin), or `brew uninstall rekcod`.");
    Ok(())
}

/// One `rekcod doctor` line: what was checked, whether it's fine, and what to do if not.
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
    fix: &'static str,
}

/// `rekcod doctor [--bundle]`.
pub fn doctor(paths: &Paths, bundle: bool) -> Result<()> {
    let mut checks = Vec::new();
    let mut add = |name, ok, detail: String, fix| checks.push(Check { name, ok, detail, fix });
    let pid = crate::cli::running_pid(paths);
    add("VM running", pid.is_some(), pid.map(|p| format!("pid {p}")).unwrap_or("not running".into()), "rekcod start");
    let ping = pid.is_some() && crate::cli::api_get(paths, "/_ping").is_ok_and(|b| b == b"OK");
    if pid.is_some() {
        add(
            "dockerd answers",
            ping,
            if ping { "yes".into() } else { "no reply on run/docker.sock".into() },
            "see ~/.rekcod/logs/console.log; rekcod stop && rekcod start",
        );
    }
    let status: serde_json::Value =
        std::fs::read(paths.status_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if let Some(v) = status["version"].as_str() {
        let same = v == env!("CARGO_PKG_VERSION");
        add("daemon version", same, format!("daemon {v}, cli {}", env!("CARGO_PKG_VERSION")), "rekcod stop && rekcod start");
    }
    let free = crate::cli::free_disk_gib(&paths.home).unwrap_or(0);
    add("free disk on the Mac", free >= 10, format!("{free} GiB"), "free some space; the VM's disk fails when the Mac's fills");
    let console = std::fs::read(paths.console_log()).unwrap_or_default();
    let disk_err = String::from_utf8_lossy(&console).lines().find(|l| l.contains("data disk error")).map(String::from);
    add("data disk", disk_err.is_none(), disk_err.unwrap_or("no errors this boot".into()), "rekcod stop && rekcod start --repair");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(paths.home.join("run")).map(|m| m.permissions().mode() & 0o777).unwrap_or(0);
    add("run/ is private", mode & 0o077 == 0, format!("{mode:o}"), "chmod 700 ~/.rekcod/run");
    if crate::docker::available() {
        let ctx = crate::docker::current_context().unwrap_or_default();
        // `default` is /var/run/docker.sock, which is rekcod once the socket is routed.
        let ours = ctx == crate::docker::CONTEXT
            || (ctx == "default" && crate::syssock::installed(paths) && std::env::var_os("DOCKER_HOST").is_none());
        add("docker context", ours, ctx, "rekcod start (or `docker context use rekcod`)");
    }
    let dh = std::env::var("DOCKER_HOST").ok();
    add("DOCKER_HOST unset", dh.is_none(), dh.unwrap_or("unset".into()), "unset DOCKER_HOST (it overrides docker contexts)");
    add(
        "/var/run/docker.sock",
        crate::syssock::installed(paths),
        if crate::syssock::installed(paths) { "routed to rekcod".into() } else { "not routed".into() },
        "rekcod socket install (for clients that ignore contexts)",
    );
    // Optional: shown, never a failure.
    add(
        "starts at login",
        true,
        if crate::service::installed(crate::cli::AUTOSTART) { "yes".into() } else { "no (`rekcod start --install` if you want it)".into() },
        "",
    );
    if ping {
        let net = crate::docker::any_cli(paths)?
            .args([
                "-H",
                &paths.docker_host(),
                "run",
                "--rm",
                "--network",
                "bridge",
                "alpine:3.22",
                "nc",
                "-z",
                "-w",
                "5",
                "1.1.1.1",
                "443",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        add(
            "VM reaches the internet",
            net,
            format!(
                "{} ({} network)",
                if net { "1.1.1.1:443 answers" } else { "a container can't connect out" },
                status["network"].as_str().unwrap_or("nat")
            ),
            if status["network"].as_str().unwrap_or("nat") == "nat" {
                "a VPN or firewall is dropping the VM's NAT traffic: `rekcod stop && rekcod start --network user` routes it through rekcod instead"
            } else {
                "check the Mac's own internet, then `rekcod stop && rekcod start`"
            },
        );
    }
    let mut bad = 0;
    for c in &checks {
        println!("{} {:<26} {}", if c.ok { "ok  " } else { "FAIL" }, c.name, c.detail);
        if !c.ok {
            bad += 1;
            println!("     fix: {}", c.fix);
        }
    }
    if bundle {
        let path = write_bundle(paths)?;
        println!("\ndiagnostics: {}", path.display());
    }
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// A tarball of status, versions, logs (last 2000 lines each) and config. No tokens or keys.
fn write_bundle(paths: &Paths) -> Result<PathBuf> {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rekcod-doctor-{t}"));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("status.json"), serde_json::to_vec_pretty(&crate::cli::status_value(paths))?)?;
    std::fs::write(dir.join("version.txt"), format!("rekcod {}\n{}\n", env!("CARGO_PKG_VERSION"), sw_vers()))?;
    if let Ok(c) = std::fs::read(paths.config()) {
        std::fs::write(dir.join("config.json"), c)?;
    }
    for e in std::fs::read_dir(paths.home.join("logs")).into_iter().flatten().flatten() {
        let text = String::from_utf8_lossy(&std::fs::read(e.path()).unwrap_or_default()).to_string();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(dir.join(e.file_name()), lines[lines.len().saturating_sub(2000)..].join("\n"))?;
    }
    let out = paths.home.join(format!("doctor-{t}.tar.gz"));
    let ok = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(std::env::temp_dir())
        .arg(dir.file_name().context("dir")?)
        .status()?
        .success();
    let _ = std::fs::remove_dir_all(&dir);
    if !ok {
        bail!("tar failed");
    }
    Ok(out)
}

fn sw_vers() -> String {
    Command::new("sw_vers").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Past this, a long-running process's log is copied to `.1` and truncated.
const LOG_MAX: u64 = 20 << 20;

/// Checks `log` every hour for as long as this process runs. Its writers opened it
/// `O_APPEND`, so after truncation they carry on at the new end.
pub fn trim_log_hourly(log: PathBuf) {
    std::thread::spawn(move || {
        loop {
            trim_log(&log, LOG_MAX);
            std::thread::sleep(Duration::from_secs(3600));
        }
    });
}

pub fn trim_log(log: &Path, max: u64) {
    if std::fs::metadata(log).is_ok_and(|m| m.len() > max) {
        let mut old = log.as_os_str().to_owned();
        old.push(".1");
        if std::fs::copy(log, &old).is_ok() {
            let _ = std::fs::OpenOptions::new().write(true).open(log).and_then(|f| f.set_len(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(parse_version("0.3.0") > parse_version("0.2.9"));
        assert!(parse_version("v1.0.0") > parse_version("0.99.0"));
        assert!(compatible("0.3.1", "0.3.0") && !compatible("0.3.0", "0.2.1"));
        assert!(compatible("1.4.0", "1.2.0") && !compatible("2.0.0", "1.9.0"));
    }

    #[test]
    fn trims_big_logs_in_place() {
        let f = std::env::temp_dir().join(format!("rekcod-log-{}.log", std::process::id()));
        std::fs::write(&f, vec![b'x'; 100]).unwrap();
        trim_log(&f, 50);
        assert_eq!(std::fs::metadata(&f).unwrap().len(), 0);
        let mut old = f.as_os_str().to_owned();
        old.push(".1");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), 100);
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_file(&old);
    }
}
