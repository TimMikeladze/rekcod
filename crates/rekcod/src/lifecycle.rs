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
        // Staged before anything is replaced: a failed copy leaves both the old binary and
        // the old dashboard.
        let dash = work.join("x/share/rekcod/dashboard");
        let installed = dir.join("../share/rekcod/dashboard");
        let staged = if dash.is_dir() && installed.parent().is_some_and(|p| p.is_dir()) {
            Some(stage_dir(&dash, &installed, DASHBOARD_FILES).context("the new dashboard")?)
        } else {
            None
        };
        // Next to the old one, then renamed over it: a VM daemon running from the old file
        // keeps its inode.
        let new = dir.join(".rekcod.new");
        if let Err(e) = std::fs::copy(work.join("x/bin/rekcod"), &new).and_then(|_| std::fs::rename(&new, &exe)) {
            let _ = std::fs::remove_file(&new);
            if let Some(s) = &staged {
                let _ = std::fs::remove_dir_all(s);
            }
            return Err(anyhow::Error::from(e).context(format!("install {}", exe.display())));
        }
        if let Some(s) = staged {
            swap_dir(&s, &installed).context("the binary is the new one, the dashboard still the old")?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    r?;
    let _ = Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&exe).stderr(Stdio::null()).status();
    println!("rekcod {latest} installed at {}. A running VM keeps {current} until `rekcod stop && rekcod start`", exe.display());
    Ok(())
}

/// What `rekcod dashboard` runs from a build (dashboard.rs); a copy without them isn't installed.
const DASHBOARD_FILES: &[&str] = &["server.js", "start.cjs"];

/// `<dir>.<ext>` next to `dir`.
fn beside(dir: &Path, ext: &str) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{ext}"));
    dir.with_file_name(name)
}

/// Copies `src` to `<dst>.new` and checks the copy has every file in `required`. Nothing
/// at `dst` changes; on failure the partial copy is removed.
fn stage_dir(src: &Path, dst: &Path, required: &[&str]) -> Result<PathBuf> {
    let staged = beside(dst, "new");
    let _ = std::fs::remove_dir_all(&staged);
    let cp = Command::new("cp").arg("-R").arg(src).arg(&staged).status().context("run cp")?;
    let missing = required.iter().find(|f| !staged.join(f).is_file());
    if cp.success() && missing.is_none() {
        return Ok(staged);
    }
    let _ = std::fs::remove_dir_all(&staged);
    if !cp.success() {
        bail!("copying it to {} failed ({cp}); {} is unchanged", staged.display(), dst.display());
    }
    bail!("it has no {}; {} is unchanged", missing.copied().unwrap_or_default(), dst.display())
}

/// Puts a staged copy at `dst`. The old `dst` moves to `<dst>.old` first and comes back if
/// the swap fails; it's deleted once the new one is in place.
fn swap_dir(staged: &Path, dst: &Path) -> Result<()> {
    let old = beside(dst, "old");
    let _ = std::fs::remove_dir_all(&old);
    let had = dst.exists();
    if had && let Err(e) = std::fs::rename(dst, &old) {
        let _ = std::fs::remove_dir_all(staged);
        return Err(e).with_context(|| format!("move {} aside", dst.display()));
    }
    if let Err(e) = std::fs::rename(staged, dst) {
        if had {
            let _ = std::fs::rename(&old, dst);
        }
        let _ = std::fs::remove_dir_all(staged);
        return Err(e).with_context(|| format!("move {} into place", staged.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// The LaunchAgents rekcod may have installed.
pub const AGENTS: &[&str] = &["vm", "menubar", "dashboard", "controller", "node", "edge", "storage"];

/// One thing `rekcod uninstall` does, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Agent sandboxes: stopped, or (`remove`) deleted with their workspaces.
    Sandboxes {
        remove: bool,
    },
    StopVm,
    RemoveJob(&'static str),
    HandBackSocket,
    SwitchContext(String),
    RemoveContext(String),
    /// `server::undo_system`: the `~/.zshenv` block, firewall exceptions, pmset.
    System,
    RemoveInclude,
    DeleteHome,
}

impl Step {
    /// A failure here means something of rekcod's still runs, so the home it runs from
    /// must stay.
    fn blocks_delete(&self) -> bool {
        matches!(self, Step::Sandboxes { .. } | Step::StopVm | Step::RemoveJob(_))
    }
}

/// What uninstall found before changing anything.
#[derive(Debug, Default)]
struct Facts {
    default_home: bool,
    keep_data: bool,
    /// Installed jobs that run with this home.
    jobs: Vec<&'static str>,
    socket: bool,
    /// The docker context is `rekcod` or a host's, and this is where it goes back to.
    switch_to: Option<String>,
    rekcod_context: bool,
    host_contexts: Vec<String>,
}

/// Machine-wide state (the `rekcod` context, `~/.zshenv`, firewall, pmset, the ssh
/// Include) belongs to `~/.rekcod`'s install: another `REKCOD_HOME` only removes its own.
fn plan(f: &Facts) -> Vec<Step> {
    let mut steps = vec![Step::Sandboxes { remove: !f.keep_data }, Step::StopVm];
    steps.extend(f.jobs.iter().map(|j| Step::RemoveJob(j)));
    if f.socket {
        steps.push(Step::HandBackSocket);
    }
    if f.default_home {
        // Host contexts need rekcod's ssh key, which only a full uninstall deletes.
        let hosts = if f.keep_data { &[][..] } else { &f.host_contexts[..] };
        if let Some(to) = &f.switch_to {
            steps.push(Step::SwitchContext(to.clone()));
        }
        if f.rekcod_context {
            steps.push(Step::RemoveContext(crate::docker::CONTEXT.into()));
        }
        steps.extend(hosts.iter().map(|h| Step::RemoveContext(h.clone())));
        steps.push(Step::System);
    }
    if !f.keep_data {
        if f.default_home {
            steps.push(Step::RemoveInclude);
        }
        steps.push(Step::DeleteHome);
    }
    steps
}

/// Runs `steps`, printing each. Once a step that [`Step::blocks_delete`] fails, the
/// Include and the home stay. Errs naming every step that failed or was held back.
fn execute(steps: &[Step], mut run: impl FnMut(&Step) -> Vec<(String, Result<()>)>) -> Result<()> {
    let mut failed: Vec<String> = Vec::new();
    let mut blocked = false;
    for step in steps {
        if blocked && matches!(step, Step::RemoveInclude | Step::DeleteHome) {
            let what = if *step == Step::DeleteHome { "delete the rekcod home" } else { "remove rekcod's Include from ~/.ssh/config" };
            println!("kept  {what}: something above is still installed or running");
            failed.push(what.into());
            continue;
        }
        for (name, r) in run(step) {
            match r {
                Ok(()) => println!("ok    {name}"),
                Err(e) => {
                    println!("FAIL  {name}: {e:#}");
                    blocked |= step.blocks_delete();
                    failed.push(name);
                }
            }
        }
    }
    if !failed.is_empty() {
        bail!("uninstall is incomplete; not done: {}. Fix what's above and run it again", failed.join("; "));
    }
    Ok(())
}

/// The rekcod home, unless deleting it would take more than rekcod's own directory.
fn deletable(home: &Path, user_home: Option<&Path>) -> Result<()> {
    if home.parent().is_none() || user_home.is_some_and(|u| u.starts_with(home)) {
        bail!("{} holds more than rekcod's data; not deleting it", home.display());
    }
    if !home.join("run").is_dir() || !home.join("vm").is_dir() {
        bail!("{} doesn't look like a rekcod home (no run/ and vm/); not deleting it", home.display());
    }
    Ok(())
}

fn facts(paths: &Paths, keep_data: bool) -> Facts {
    let default_home = paths.is_default_home();
    let jobs = AGENTS.iter().copied().filter(|a| crate::service::installed_here(paths, a)).collect();
    let mut f = Facts { default_home, keep_data, jobs, socket: crate::syssock::installed(paths), ..Default::default() };
    if default_home && crate::docker::available() {
        f.rekcod_context = crate::docker::context_exists(crate::docker::CONTEXT);
        f.host_contexts = crate::host::contexts();
        let current = crate::docker::current_context().unwrap_or_default();
        let leaving = current == crate::docker::CONTEXT || (!keep_data && f.host_contexts.contains(&current));
        if leaving {
            let prev = std::fs::read_to_string(paths.prev_context()).unwrap_or_default().trim().to_string();
            let ours = prev == crate::docker::CONTEXT || prev.starts_with("rekcod-");
            f.switch_to = Some(if !prev.is_empty() && !ours && crate::docker::context_exists(&prev) { prev } else { "default".into() });
        }
    }
    f
}

/// `rekcod uninstall [--keep-data] [--yes]`.
pub fn uninstall(paths: &Paths, keep_data: bool, yes: bool) -> Result<()> {
    let f = facts(paths, keep_data);
    let home = paths.home.display();
    if !f.default_home {
        eprintln!(
            "rekcod: REKCOD_HOME is {home}: this removes only that home's VM, sandboxes and jobs. The docker context, \
             ~/.zshenv, firewall and pmset belong to ~/.rekcod's install and stay."
        );
    }
    if !keep_data {
        deletable(&paths.home, std::env::var_os("HOME").map(PathBuf::from).as_deref())?;
    }
    let what = if keep_data {
        format!("stop rekcod and its agent sandboxes (kept, with their work), remove its jobs and docker context, and keep {home}")
    } else {
        format!("stop rekcod, remove its jobs and docker contexts, and delete {home}: VM disks, volumes, images, snapshots, sandboxes")
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
    // Ask for sudo before anything changes: without it the daemons would stay loaded,
    // running from a home this deletes.
    let daemons: Vec<&str> = f.jobs.iter().copied().filter(|j| crate::service::is_daemon(j)).collect();
    if !daemons.is_empty() && !crate::service::can_install_daemons() {
        bail!(
            "{} {} LaunchDaemons, and removing them needs sudo, which can't ask here; nothing changed. Run it in a terminal",
            daemons.join(", "),
            if daemons.len() == 1 { "is a" } else { "are" }
        );
    }
    let one = |name: String, r: Result<()>| vec![(name, r)];
    let r = execute(&plan(&f), |step| match step {
        Step::Sandboxes { remove: true } => one("remove agent sandboxes".into(), crate::sandbox::rm(paths, &[], true)),
        Step::Sandboxes { remove: false } => one("stop agent sandboxes".into(), stop_sandboxes(paths)),
        Step::StopVm => one("stop the VM".into(), crate::cli::stop(paths, true, false)),
        Step::RemoveJob(j) => one(format!("remove {}", crate::service::label(j)), crate::service::uninstall(j)),
        Step::HandBackSocket => one("hand back /var/run/docker.sock".into(), crate::syssock::uninstall(paths)),
        Step::SwitchContext(to) => one(format!("switch docker context to {to}"), crate::docker::use_context(to)),
        Step::RemoveContext(c) => one(format!("remove docker context {c}"), crate::docker::run_ok(&["context", "rm", "-f", c]).map(|_| ())),
        Step::System => crate::server::undo_system(paths),
        Step::RemoveInclude => one("remove rekcod's Include from ~/.ssh/config".into(), crate::sshkey::remove_include()),
        Step::DeleteHome => one(format!("delete {home}"), std::fs::remove_dir_all(&paths.home).map_err(Into::into)),
    });
    leftovers(paths, keep_data);
    r
}

/// Each running agent sandbox stopped; its directory (workspace, reviewed copy, disks) stays.
fn stop_sandboxes(paths: &Paths) -> Result<()> {
    let mut failed = Vec::new();
    for s in crate::sandbox::list(paths).into_iter().filter(|s| s.running) {
        if let Err(e) = crate::sandbox::stop(paths, &s.meta.id) {
            failed.push(format!("{}: {e:#}", s.meta.id));
        }
    }
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    Ok(())
}

/// What uninstall leaves for the user to remove, with the commands.
fn leftovers(paths: &Paths, keep_data: bool) {
    let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "rekcod".into());
    // One that `host add` copied lives in ~/.rekcod/bin and just went with it.
    if keep_data || !Path::new(&exe).starts_with(&paths.home) {
        println!("\nThe binary is still at {exe}; delete it with `rm {exe}` (sudo if it's in /usr/local/bin), or `brew uninstall rekcod`.");
    }
    if !paths.is_default_home() {
        return;
    }
    let pkg =
        Command::new("/usr/sbin/pkgutil").args(["--pkg-info", "dev.rekcod.server"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    if pkg.is_ok_and(|s| s.success()) {
        println!(
            "The server pkg's files stay: the guest, dashboard and domain in /usr/local/share/rekcod and its install log. Remove them with\n  \
             sudo rm -rf /usr/local/share/rekcod /var/log/rekcod-server-install.log && sudo pkgutil --forget dev.rekcod.server"
        );
    }
    if let Some(h) = std::env::var_os("HOME") {
        for p in ["Desktop/rekcod-pair.txt", "rekcod-pair.txt"].map(|p| PathBuf::from(&h).join(p)) {
            if p.exists() {
                println!("{} holds the old fleet's admin token; delete it.", p.display());
            }
        }
    }
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
    // Paths::at already took group and other access off these; what's left it couldn't.
    let open = paths.not_private();
    add(
        "state is private",
        open.is_empty(),
        if open.is_empty() { "home, run/, logs/ and vm/ 700, VM disks 600".into() } else { open.join(", ") },
        "sudo chown -R \"$(whoami)\" ~/.rekcod, then any rekcod command sets the modes again",
    );
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
                "alpine:3.24",
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

    /// A dashboard build whose files all say `tag`.
    fn build(dir: &Path, tag: &str) {
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        for f in DASHBOARD_FILES.iter().chain(&["lib/chunk.js"]) {
            std::fs::write(dir.join(f), tag).unwrap();
        }
    }

    #[test]
    fn a_failed_dashboard_copy_keeps_the_installed_one() {
        use std::os::unix::fs::PermissionsExt;
        let t = std::env::temp_dir().join(format!("rekcod-upgrade-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (installed, new) = (t.join("share/dashboard"), t.join("x/dashboard"));
        build(&installed, "old");
        build(&new, "new");
        let kept = |installed: &Path| {
            assert_eq!(std::fs::read_to_string(installed.join("server.js")).unwrap(), "old");
            assert!(!beside(installed, "new").exists(), "the partial copy is gone");
        };

        // cp can't read this one, so the copy fails part way (root reads it anyway).
        let unreadable = new.join("lib/unreadable.js");
        std::fs::write(&unreadable, "x").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(stage_dir(&new, &installed, DASHBOARD_FILES).is_err());
            kept(&installed);
        }
        std::fs::remove_file(&unreadable).unwrap();

        std::fs::remove_file(new.join("start.cjs")).unwrap();
        let err = stage_dir(&new, &installed, DASHBOARD_FILES).unwrap_err();
        assert!(err.to_string().contains("start.cjs"), "{err}");
        kept(&installed);

        build(&new, "new");
        swap_dir(&stage_dir(&new, &installed, DASHBOARD_FILES).unwrap(), &installed).unwrap();
        assert_eq!(std::fs::read_to_string(installed.join("lib/chunk.js")).unwrap(), "new");
        assert!(!beside(&installed, "new").exists() && !beside(&installed, "old").exists());

        // A first install has nothing to move aside.
        let first = t.join("share/first");
        swap_dir(&stage_dir(&new, &first, DASHBOARD_FILES).unwrap(), &first).unwrap();
        assert!(first.join("start.cjs").is_file());
        std::fs::remove_dir_all(&t).unwrap();
    }

    fn facts(default_home: bool, keep_data: bool) -> Facts {
        Facts {
            default_home,
            keep_data,
            jobs: vec!["vm", "controller"],
            socket: true,
            switch_to: Some("default".into()),
            rekcod_context: true,
            host_contexts: vec!["rekcod-mini".into()],
        }
    }

    #[test]
    fn uninstall_plans() {
        use Step::*;
        let full = plan(&facts(true, false));
        assert_eq!(
            full,
            [
                Sandboxes { remove: true },
                StopVm,
                RemoveJob("vm"),
                RemoveJob("controller"),
                HandBackSocket,
                SwitchContext("default".into()),
                RemoveContext("rekcod".into()),
                RemoveContext("rekcod-mini".into()),
                System,
                RemoveInclude,
                DeleteHome
            ]
        );
        // --keep-data stops sandboxes instead of deleting them, and hosts keep their contexts.
        let keep = plan(&facts(true, true));
        assert_eq!(keep[0], Sandboxes { remove: false });
        assert!(!keep.contains(&DeleteHome) && !keep.contains(&RemoveContext("rekcod-mini".into())));
        // Another REKCOD_HOME touches nothing machine-wide.
        let other = plan(&facts(false, false));
        assert!(other.iter().all(|s| !matches!(s, SwitchContext(_) | RemoveContext(_) | System | RemoveInclude)), "{other:?}");
    }

    /// A job that couldn't be removed keeps the home it runs from, and the result is an error.
    #[test]
    fn a_failed_step_keeps_the_home() {
        let deleted = std::cell::Cell::new(false);
        let r = execute(&plan(&facts(true, false)), |s| {
            if *s == Step::DeleteHome {
                deleted.set(true);
            }
            let r = if *s == Step::RemoveJob("controller") { Err(anyhow!("sudo: a password is required")) } else { Ok(()) };
            vec![(format!("{s:?}"), r)]
        });
        assert!(!deleted.get());
        let e = r.unwrap_err().to_string();
        assert!(e.contains("controller") && e.contains("delete the rekcod home"), "{e}");
        assert!(execute(&plan(&facts(true, false)), |s| vec![(format!("{s:?}"), Ok(()))]).is_ok());
    }

    #[test]
    fn only_a_rekcod_home_is_deleted() {
        let t = std::env::temp_dir().join(format!("rekcod-del-{}", std::process::id()));
        let home = t.join("h");
        std::fs::create_dir_all(home.join("run")).unwrap();
        assert!(deletable(&home, None).is_err(), "no vm/");
        std::fs::create_dir_all(home.join("vm")).unwrap();
        deletable(&home, Some(Path::new("/Users/me"))).unwrap();
        assert!(deletable(&home, Some(&home.join("me"))).is_err(), "the user's home is inside it");
        assert!(deletable(Path::new("/"), None).is_err());
        std::fs::remove_dir_all(&t).unwrap();
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
