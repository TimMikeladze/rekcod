//! Upgrading, removing and diagnosing rekcod.

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

/// A node or edge at `peer` may still talk to a controller at `ours`: [`compatible`], or
/// one minor version behind while major is 0. Servers upgrade controller first, then
/// nodes and edges, so those are never ahead.
pub fn compatible_peer(peer: &str, ours: &str) -> bool {
    let (p, o) = (parse_version(peer), parse_version(ours));
    compatible(peer, ours) || (p.0 == 0 && o.0 == 0 && p.1 + 1 == o.1)
}

/// The GitHub API call: small, so a total cap is fine here.
fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "--tlsv1.2", "--connect-timeout", "15", "--max-time", "60"])
        .args(args)
        .output()
        .context("run curl")?;
    if !out.status.success() {
        bail!("curl {}: {}", args.last().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// A release file into `dest`, without a cap on the whole transfer (the tarball carries
/// the dashboard; a slow link still finishes), giving up on a stalled one.
fn download(url: &str, dest: &Path, https_only: bool) -> Result<()> {
    let out = crate::fetch::download_cmd(url, dest, https_only).arg("-sS").output().context("run curl")?;
    if !out.status.success() {
        bail!("download {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn latest_version() -> Result<String> {
    let body = curl(&["-H", "Accept: application/vnd.github+json", &format!("https://api.github.com/repos/{REPO}/releases/latest")])?;
    let v: serde_json::Value = serde_json::from_slice(&body)?;
    v["tag_name"].as_str().map(|t| t.trim_start_matches('v').to_string()).ok_or_else(|| anyhow!("no tag in GitHub's latest release"))
}

/// The release base, and whether it's GitHub's (https only): `REKCOD_RELEASE_URL` may
/// point at a local server or a file:// mirror.
fn release_base(version: &str) -> (String, bool) {
    match std::env::var("REKCOD_RELEASE_URL") {
        Ok(b) => (b.trim_end_matches('/').to_string(), false),
        Err(_) => (format!("https://github.com/{REPO}/releases/download/v{version}"), true),
    }
}

/// `upgrade --check`'s exit code: 0 up to date, 1 a newer release exists, 2 couldn't tell.
fn check_code(latest: &Result<String>, current: &str) -> i32 {
    match latest {
        Ok(l) if parse_version(l) > parse_version(current) => 1,
        Ok(_) => 0,
        Err(_) => 2,
    }
}

/// The Sigstore bundle the release workflow publishes beside the tarball (since 0.3.1).
const BUNDLE: &str = "rekcod.sigstore.json";
const SIGNER_WORKFLOW: &str = "TimMikeladze/rekcod/.github/workflows/release.yml";

#[derive(Debug, PartialEq, Eq)]
enum Provenance {
    Verified,
    /// gh couldn't check (not logged in, no network): said, not refused.
    Unchecked(String),
}

/// `gh attestation verify` on the tarball: the build provenance GitHub's release workflow
/// signed, which whoever can replace a release asset can't forge. A failed verification
/// refuses the download; gh being unable to check is reported.
fn attestation(gh: &Path, tarball: &Path, bundle: Option<&Path>) -> Result<Provenance> {
    let verify = |bundle: Option<&Path>| -> Result<std::process::Output> {
        let mut c = as_invoking_user(gh);
        c.args(["attestation", "verify"]).arg(tarball).args(["--repo", REPO, "--signer-workflow", SIGNER_WORKFLOW]);
        if let Some(b) = bundle {
            c.arg("--bundle").arg(b);
        }
        c.stdin(Stdio::null()).output().context("run gh")
    };
    let mut out = verify(bundle)?;
    // A bundle asset that doesn't load is no verdict: the lookup on GitHub can't be swapped
    // along with the release's files.
    if !out.status.success() && bundle.is_some() {
        out = verify(None)?;
    }
    if out.status.success() {
        return Ok(Provenance::Verified);
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let lower = err.to_lowercase();
    if ["gh auth login", "authentication", "not logged", "http 401", "could not resolve", "no such host", "timeout"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return Ok(Provenance::Unchecked(err.lines().last().unwrap_or_default().to_string()));
    }
    bail!("{} has no valid build provenance from {REPO}'s release workflow; not installing it\n{err}", tarball.display())
}

/// `bin` as the user who ran `sudo rekcod upgrade`: gh reads that user's login, and as
/// root it would leave root-owned files in their home.
fn as_invoking_user(bin: &Path) -> Command {
    let sudo_user = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty() && u != "root");
    match sudo_user {
        Some(u) if unsafe { libc::geteuid() } == 0 => {
            let mut c = Command::new("/usr/bin/sudo");
            c.args(["-u", &u, "-H", "--"]).arg(bin);
            c
        }
        _ => Command::new(bin),
    }
}

/// The Developer ID team that signed `bin`, if one did (`TeamIdentifier=` in `codesign -dv`).
fn team_of(bin: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/codesign").arg("-dv").arg(bin).output().ok()?;
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .filter(|t| *t != "not set")
        .map(String::from)
}

/// Whether `bin` carries a valid signature from Developer ID team `team`.
fn signed_by(bin: &Path, team: &str) -> bool {
    let req = format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"");
    Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-R", &req])
        .arg(bin)
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `.rekcod.previous` beside the binary: what the last upgrade replaced.
fn previous_of(exe: &Path) -> PathBuf {
    exe.with_file_name(".rekcod.previous")
}

/// The binary `upgrade` replaces, or why it won't.
fn upgradable_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe.components().any(|c| c.as_os_str() == "Cellar") {
        bail!("this rekcod came from Homebrew; `brew upgrade rekcod`");
    }
    let dir = exe.parent().context("binary has no directory")?;
    let probe = dir.join(".rekcod-upgrade-probe");
    if std::fs::write(&probe, b"").is_err() {
        bail!("can't write to {}; run it with sudo", dir.display());
    }
    let _ = std::fs::remove_file(&probe);
    Ok(exe)
}

/// Whether `upgrade --rollback` has something to go back to.
pub fn rollback_available() -> bool {
    std::env::current_exe().and_then(|e| e.canonicalize()).is_ok_and(|e| previous_of(&e).is_file())
}

/// `rekcod upgrade --rollback`: the binary the last upgrade replaced goes back in place, and
/// the current one becomes `.rekcod.previous` (a second rollback goes forward again). The
/// dashboard stays as the upgrade left it.
pub fn rollback() -> Result<()> {
    let exe = upgradable_exe()?;
    let prev = previous_of(&exe);
    if !prev.is_file() {
        bail!("nothing to roll back to: {} doesn't exist (an upgrade keeps the binary it replaces there)", prev.display());
    }
    swap_binaries(&exe, &prev)?;
    let v = Command::new(&exe).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    println!("{} is {v} again; {} is the one it replaced", exe.display(), prev.display());
    restart_fleet(&exe);
    Ok(())
}

/// `exe` and `other` trade places, each move a rename so a running daemon keeps its inode.
fn swap_binaries(exe: &Path, other: &Path) -> Result<()> {
    let tmp = exe.with_file_name(".rekcod.swap");
    let _ = std::fs::remove_file(&tmp);
    std::fs::hard_link(exe, &tmp).context("keep the current binary")?;
    if let Err(e) = std::fs::rename(other, exe) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("put {} in place", other.display()));
    }
    std::fs::rename(&tmp, other).context("keep the replaced binary")?;
    Ok(())
}

/// `rekcod upgrade [--check]`.
pub fn upgrade(check: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = latest_version();
    if check {
        let code = check_code(&latest, current);
        match &latest {
            Ok(l) if code == 1 => println!("rekcod {l} is out (this is {current}); `rekcod upgrade` installs it"),
            Ok(_) => println!("rekcod {current} is the latest"),
            Err(e) => eprintln!("rekcod: couldn't check for a newer release: {e:#}"),
        }
        std::process::exit(code);
    }
    let latest = latest?;
    if parse_version(&latest) <= parse_version(current) {
        println!("rekcod {current} is the latest");
        return Ok(());
    }
    let exe = upgradable_exe()?;
    let dir = exe.parent().context("binary has no directory")?.to_path_buf();
    let work = std::env::temp_dir().join(format!("rekcod-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("x"))?;
    let mut checked: Vec<String> = Vec::new();
    let r = (|| -> Result<()> {
        let (base, https_only) = release_base(&latest);
        eprintln!("rekcod: downloading {latest}");
        let tar = work.join(TARBALL);
        download(&format!("{base}/{TARBALL}"), &tar, https_only)?;
        let sums_file = work.join("SHA256SUMS");
        download(&format!("{base}/SHA256SUMS"), &sums_file, https_only)?;
        let sums = std::fs::read_to_string(&sums_file)?;
        let want =
            sums.lines().find(|l| l.ends_with(TARBALL)).and_then(|l| l.split_whitespace().next()).context("SHA256SUMS lists no tarball")?;
        let got =
            crate::fetch::parse_shasum(&String::from_utf8_lossy(&Command::new("shasum").args(["-a", "256"]).arg(&tar).output()?.stdout))
                .map(str::to_string)
                .unwrap_or_default();
        if !got.eq_ignore_ascii_case(want) {
            bail!("checksum mismatch: SHA256SUMS says {want}, the download is {got}; not installing it");
        }
        // The checksum comes from the same place as the tarball; these don't.
        match which("gh") {
            Some(gh) => {
                let bundle = work.join(BUNDLE);
                let bundle = download(&format!("{base}/{BUNDLE}"), &bundle, https_only).is_ok().then_some(bundle);
                match attestation(&gh, &tar, bundle.as_deref())? {
                    Provenance::Verified => checked.push(format!("build provenance from {REPO}'s release workflow")),
                    Provenance::Unchecked(why) => eprintln!("rekcod: gh couldn't check the build provenance ({why})"),
                }
            }
            None => eprintln!("rekcod: no gh, so the build provenance wasn't checked (`brew install gh` adds that check)"),
        }
        if !Command::new("tar").arg("-xzf").arg(&tar).arg("-C").arg(work.join("x")).status()?.success() {
            bail!("couldn't unpack {TARBALL}");
        }
        let new_bin = work.join("x/bin/rekcod");
        if let Some(team) = team_of(&exe) {
            if !signed_by(&new_bin, &team) {
                bail!("the new binary isn't signed by Developer ID team {team}, which signed this one; not installing it");
            }
            checked.push(format!("Developer ID signature of team {team}"));
        }
        if checked.is_empty() {
            eprintln!("rekcod: only the checksum was checked, and it comes from the same release as the download");
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
        // keeps its inode. The old one stays as .rekcod.previous for `--rollback`.
        let new = dir.join(".rekcod.new");
        let prev = previous_of(&exe);
        let _ = std::fs::remove_file(&prev);
        let installed_bin =
            std::fs::copy(&new_bin, &new).and_then(|_| std::fs::hard_link(&exe, &prev)).and_then(|_| std::fs::rename(&new, &exe));
        if let Err(e) = installed_bin {
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
    if !checked.is_empty() {
        println!("verified: {}", checked.join(", "));
    }
    println!(
        "rekcod {latest} installed at {}; {current} is kept as {} (`rekcod upgrade --rollback`). A running VM keeps {current} until `rekcod stop && rekcod start`",
        exe.display(),
        previous_of(&exe).display()
    );
    restart_fleet(&exe);
    Ok(())
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Restarts the fleet services installed on this Mac that run `exe`, in
/// [`crate::service::FLEET_SERVICES`] order, so they run the new binary. Once the
/// controller is restarted, the rest wait until it listens again.
fn restart_fleet(exe: &Path) {
    for name in crate::service::FLEET_SERVICES {
        let Some(argv) = crate::service::job_args(name) else { continue };
        if argv.first().and_then(|a| Path::new(a).canonicalize().ok()).as_deref() != Some(exe) {
            eprintln!("rekcod: {name} runs {}, not this binary; leaving it alone", argv.first().map_or("?", String::as_str));
            continue;
        }
        eprintln!("rekcod: restarting {name}");
        if let Err(e) = crate::service::restart(name) {
            eprintln!("rekcod: {name}: {e:#}");
            continue;
        }
        // It waits up to 30 s for deploys in flight before it exits.
        if *name == "controller"
            && let Some(addr) = listen_addr(&argv)
            && !wait_for_port(&addr, Duration::from_secs(90))
        {
            eprintln!("rekcod: the controller isn't listening on {addr} yet; restarting the rest anyway");
        }
    }
}

/// Where a service's `--listen` points, as an address to connect to.
fn listen_addr(argv: &[String]) -> Option<String> {
    let l = argv.iter().skip_while(|a| *a != "--listen").nth(1)?;
    let (host, port) = l.rsplit_once(':')?;
    let host = match host.trim_matches(['[', ']']) {
        "0.0.0.0" | "::" | "" => "127.0.0.1",
        h => h,
    };
    Some(if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") })
}

fn wait_for_port(addr: &str, within: Duration) -> bool {
    use std::net::ToSocketAddrs;
    let t0 = std::time::Instant::now();
    while t0.elapsed() < within {
        let up =
            addr.to_socket_addrs().into_iter().flatten().any(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(1)).is_ok());
        if up {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
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

/// The rekcod home, unless deleting it would take more than rekcod's own directory: one
/// rekcod created (its marker), or one holding only what rekcod puts in a home.
fn deletable(home: &Path, user_home: Option<&Path>) -> Result<()> {
    if home.parent().is_none() || user_home.is_some_and(|u| u.starts_with(home)) {
        bail!("{} holds more than rekcod's data; not deleting it", home.display());
    }
    if home.join(crate::paths::HOME_MARKER).exists() {
        return Ok(());
    }
    let strangers: Vec<String> = std::fs::read_dir(home)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            !crate::paths::HOME_ENTRIES.contains(&n.as_str())
                && !n.starts_with("doctor-")
                && !n.starts_with("dev.rekcod.")
                && !n.ends_with(".tmp")
        })
        .collect();
    if !strangers.is_empty() {
        bail!(
            "{} has files rekcod didn't put there ({}); not deleting it. Move them out, or use --keep-data and delete it yourself",
            home.display(),
            strangers.join(", ")
        );
    }
    Ok(())
}

fn facts(paths: &Paths, keep_data: bool) -> Facts {
    let default_home = paths.is_default_home();
    let jobs = AGENTS.iter().copied().filter(|a| crate::service::installed_here(paths, a)).collect();
    let mut f = Facts { default_home, keep_data, jobs, socket: crate::syssock::present(paths), ..Default::default() };
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
#[derive(Debug, serde::Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
    fix: &'static str,
}

/// What the docker side of `doctor` looked at.
struct DockerSeen {
    /// `DOCKER_HOST` in this environment.
    docker_host: Option<String>,
    /// `unix://<home>/run/docker.sock`: what `host add`'s ~/.zshenv block exports.
    ours: String,
    /// `/var/run/docker.sock` goes to rekcod.
    socket_routed: bool,
    /// It did once (`socket install`), and doesn't now.
    socket_lost: bool,
    /// `syssock::state`, said when it isn't routed.
    socket_state: String,
    /// The current docker context, when a docker CLI is there.
    context: Option<String>,
}

/// The checks that depend only on the environment. Optional setup (the socket route,
/// starting at login) is shown, never a failure, unless a route that was set up is gone.
fn docker_checks(s: &DockerSeen) -> Vec<Check> {
    let mut out = Vec::new();
    let dh_ours = s.docker_host.as_deref() == Some(s.ours.as_str());
    if let Some(ctx) = &s.context {
        // `default` is /var/run/docker.sock, which is rekcod once the socket is routed; a
        // DOCKER_HOST at rekcod's socket overrides any context.
        let ours = dh_ours || ctx == crate::docker::CONTEXT || (ctx == "default" && s.socket_routed && s.docker_host.is_none());
        out.push(Check { name: "docker context", ok: ours, detail: ctx.clone(), fix: "rekcod start (or `docker context use rekcod`)" });
    }
    out.push(Check {
        name: "DOCKER_HOST",
        ok: s.docker_host.is_none() || dh_ours,
        detail: match &s.docker_host {
            None => "unset".into(),
            Some(_) if dh_ours => "rekcod's socket".into(),
            Some(h) => h.clone(),
        },
        fix: "unset DOCKER_HOST (it overrides docker contexts)",
    });
    out.push(Check {
        name: "/var/run/docker.sock",
        ok: !s.socket_lost,
        detail: if s.socket_routed {
            "routed to rekcod".into()
        } else if s.socket_lost {
            s.socket_state.clone()
        } else {
            "not routed (optional: `rekcod socket install`, for clients that ignore contexts)".into()
        },
        fix: "rekcod socket install",
    });
    out
}

/// `rekcod doctor [--bundle] [--json]`.
pub fn doctor(paths: &Paths, bundle: bool, json: bool) -> Result<()> {
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
    if let (Some(g), Some(want)) = (status["guest_version"].as_str().filter(|g| !g.is_empty()), crate::fetch::GUEST_VERSION) {
        add("guest version", g == want, format!("running {g}, this build's {want}"), "rekcod stop && rekcod start");
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
    // Other Macs' nodes and apps reach RustFS directly; the firewall would accept and stall them.
    if let Some((ok, what)) = crate::server::storage_firewall(paths) {
        add(
            "firewall lets RustFS through",
            ok,
            what,
            "sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add ~/.rekcod/bin/rustfs-* && sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp ~/.rekcod/bin/rustfs-*",
        );
    }
    let certs = crate::fleet::cert_expiries(paths);
    if !certs.is_empty() {
        let soon = crate::fleet::expiring(&certs, crate::fleet::now());
        add(
            "fleet certificates",
            soon.is_empty(),
            if soon.is_empty() {
                format!("{} valid for 90 days or more", certs.iter().map(|c| c.0).collect::<Vec<_>>().join(" and "))
            } else {
                soon.join(", ")
            },
            "node: check logs/node.log for renewal errors (it renews through its heartbeat); controller: rekcod server restart",
        );
    }
    let refused = crate::fleet::refused_cas(paths);
    add(
        "fleet CA pins",
        refused.is_empty(),
        if refused.is_empty() { "one certificate each".into() } else { format!("more than one certificate: {}", refused.join(", ")) },
        "rekcod remote add NAME URL --token T --ca-fingerprint sha256:... again (node.json: rekcod node join again)",
    );
    checks.extend(docker_checks(&DockerSeen {
        docker_host: std::env::var("DOCKER_HOST").ok(),
        ours: paths.docker_host(),
        socket_routed: crate::syssock::installed(paths),
        socket_lost: crate::syssock::lost(paths),
        socket_state: crate::syssock::state(paths).into(),
        context: crate::docker::available().then(|| crate::docker::current_context().unwrap_or_default()),
    }));
    // Optional: shown, never a failure.
    checks.push(Check {
        name: "starts at login",
        ok: true,
        detail: if crate::service::installed(crate::cli::AUTOSTART) {
            "yes".into()
        } else {
            "no (`rekcod start --install` if you want it)".into()
        },
        fix: "",
    });
    if ping {
        checks.push(internet_check(paths, &status)?);
    }
    let bad = checks.iter().filter(|c| !c.ok).count();
    if json {
        println!("{}", serde_json::json!({"ok": bad == 0, "checks": checks}));
    } else {
        for c in &checks {
            println!("{} {:<26} {}", if c.ok { "ok  " } else { "FAIL" }, c.name, c.detail);
            if !c.ok {
                println!("     fix: {}", c.fix);
            }
        }
    }
    if bundle {
        let path = write_bundle(paths)?;
        if json {
            eprintln!("diagnostics: {}", path.display());
        } else {
            println!("\ndiagnostics: {}", path.display());
        }
    }
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// A container connects out. Only with `alpine:3.24` already there (doctor doesn't pull),
/// and given up after 30 s.
fn internet_check(paths: &Paths, status: &serde_json::Value) -> Result<Check> {
    let nat = status["network"].as_str().unwrap_or("nat") == "nat";
    let mode = status["network"].as_str().unwrap_or("nat");
    let docker = |args: &[&str]| -> Result<Command> {
        let mut c = crate::docker::any_cli(paths)?;
        c.args(["-H", &paths.docker_host()]).args(args).stdin(Stdio::null());
        Ok(c)
    };
    let have =
        docker(&["image", "inspect", "alpine:3.24"])?.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !have {
        return Ok(Check {
            name: "VM reaches the internet",
            ok: true,
            detail: "not checked (needs alpine:3.24; `docker pull alpine:3.24` and run doctor again)".into(),
            fix: "",
        });
    }
    let probe = docker(&["run", "--rm", "--network", "bridge", "alpine:3.24", "nc", "-z", "-w", "5", "1.1.1.1", "443"])?;
    let (net, detail) = match crate::host::output_within(probe, Duration::from_secs(30)) {
        Ok(o) if o.status.success() => (true, format!("1.1.1.1:443 answers ({mode} network)")),
        Ok(_) => (false, format!("a container can't connect out ({mode} network)")),
        Err(e) => (false, format!("the probe container didn't finish: {e:#} ({mode} network)")),
    };
    Ok(Check {
        name: "VM reaches the internet",
        ok: net,
        detail,
        fix: if nat {
            "a VPN or firewall is dropping the VM's NAT traffic: `rekcod stop && rekcod start --network user` routes it through rekcod instead"
        } else {
            "check the Mac's own internet, then `rekcod stop && rekcod start`"
        },
    })
}

/// `text` with the credentials taken out of every URL in it: an http(s) URL's user part
/// (`https://TOKEN@host` is a token too), and any `user:password@`. `ssh://git@host` stays.
/// A git URL with a token in it otherwise reaches node logs, viewers and doctor bundles.
pub fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let scheme = head[..i].rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '+').next().unwrap_or("").to_ascii_lowercase();
        let end = tail.find(|c: char| c.is_whitespace() || "/?#\"'<>".contains(c)).unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(at) if scheme.starts_with("http") || tail[..at].contains(':') => {
                out.push_str("***@");
                rest = &tail[at + 1..];
            }
            _ => rest = tail,
        }
    }
    out.push_str(rest);
    out
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
        let text = redact_userinfo(&String::from_utf8_lossy(&std::fs::read(e.path()).unwrap_or_default()));
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

/// launchd starts agents with a soft limit of 256 descriptors; every proxied or forwarded
/// connection takes two. Raises the soft limit as far as the system allows and returns it.
pub fn raise_fd_limit() -> u64 {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return 256;
    }
    let per_proc = crate::cli::sysctl_u64("kern.maxfilesperproc").unwrap_or(10240) as libc::rlim_t;
    let want = lim.rlim_max.min(per_proc);
    if want > lim.rlim_cur {
        let raised = libc::rlimit { rlim_cur: want, rlim_max: lim.rlim_max };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            return want;
        }
    }
    lim.rlim_cur
}

/// Checks `log` every hour for as long as this process runs. Its writers opened it
/// `O_APPEND`, so after truncation they carry on at the new end.
pub fn trim_log_hourly(log: PathBuf) {
    trim_log_every(log, Duration::from_secs(3600));
}

/// [`trim_log_hourly`] for a log something else can write fast (a VM's daemon).
pub fn trim_log_every(log: PathBuf, period: Duration) {
    std::thread::spawn(move || {
        loop {
            trim_log(&log, LOG_MAX);
            std::thread::sleep(period);
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

    /// Setups rekcod made itself pass: DOCKER_HOST at rekcod's socket (host add's
    /// ~/.zshenv block), and no /var/run/docker.sock route.
    #[test]
    fn doctor_passes_rekcods_own_setups() {
        let ours = "unix:///Users/me/.rekcod/run/docker.sock".to_string();
        let seen = |dh: Option<&str>, ctx: &str| DockerSeen {
            docker_host: dh.map(String::from),
            ours: ours.clone(),
            socket_routed: false,
            socket_lost: false,
            socket_state: String::new(),
            context: Some(ctx.into()),
        };
        for s in [seen(Some(&ours), "default"), seen(None, "rekcod")] {
            let failed: Vec<_> = docker_checks(&s).into_iter().filter(|c| !c.ok).map(|c| c.name).collect();
            assert!(failed.is_empty(), "{failed:?}");
        }
        let other = docker_checks(&seen(Some("tcp://10.0.0.9:2375"), "rekcod"));
        assert!(other.iter().any(|c| c.name == "DOCKER_HOST" && !c.ok));
        assert!(!docker_checks(&seen(None, "orbstack"))[0].ok);
    }

    #[test]
    fn credentials_come_out_of_urls() {
        let line = "rekcod: blog r3: cloning https://x-access-token:ghp_secret@github.com/org/repo.git at main";
        assert_eq!(redact_userinfo(line), "rekcod: blog r3: cloning https://***@github.com/org/repo.git at main");
        assert_eq!(redact_userinfo("git ssh://git@host/r http://a:b@c:1/x?y=z@w"), "git ssh://git@host/r http://***@c:1/x?y=z@w");
        assert_eq!(redact_userinfo("(https://ghp_x@github.com/o/r)"), "(https://***@github.com/o/r)");
        assert_eq!(redact_userinfo("ssh://me:pw@host/r"), "ssh://***@host/r");
        assert_eq!(redact_userinfo("https://github.com/org/repo, me@example.com"), "https://github.com/org/repo, me@example.com");
    }

    #[test]
    fn check_exit_codes() {
        assert_eq!(check_code(&Ok("0.4.0".into()), "0.3.0"), 1);
        assert_eq!(check_code(&Ok("0.3.0".into()), "0.3.0"), 0);
        // A network error is not "a newer release exists".
        assert_eq!(check_code(&Err(anyhow!("could not resolve api.github.com")), "0.3.0"), 2);
    }

    /// A stand-in gh: verified, refused, or unable to check.
    #[test]
    fn provenance_refuses_only_a_failed_verification() {
        let dir = std::env::temp_dir().join(format!("rekcod-gh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        let fake = |script: &str| {
            std::fs::write(&gh, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            attestation(&gh, Path::new("/x.tar.gz"), Some(Path::new("/b.json")))
        };
        let args = dir.join("args");
        let ok = fake(&format!("echo \"$@\" > {}; exit 0", args.display())).unwrap();
        assert_eq!(ok, Provenance::Verified);
        let a = std::fs::read_to_string(&args).unwrap();
        assert!(a.contains("--repo TimMikeladze/rekcod") && a.contains("--signer-workflow") && a.contains("--bundle /b.json"), "{a}");
        // A bundle gh can't read: the online lookup decides.
        let online = fake("case \"$*\" in *--bundle*) echo 'failed to load bundle' >&2; exit 1;; esac; exit 0").unwrap();
        assert_eq!(online, Provenance::Verified);
        assert!(fake("echo 'Error: verifying with issuer \"sigstore.dev\": no matching attestations' >&2; exit 1").is_err());
        assert!(matches!(
            fake("echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4").unwrap(),
            Provenance::Unchecked(_)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The test binary is ad-hoc signed, like a binary someone swapped into a release
    /// without the team's key: it isn't accepted as the team's.
    #[test]
    fn a_binary_not_signed_by_the_team_is_refused() {
        let me = std::env::current_exe().unwrap();
        assert_eq!(team_of(&me), None);
        assert!(!signed_by(&me, "ABCDE12345"));
        // And a team-signed one passes its own team's requirement, so the requirement
        // itself parses (a malformed one would refuse everything too).
        let signed = ["/Applications/Google Chrome.app", "/Applications/Docker.app", "/Applications/Slack.app"]
            .iter()
            .map(Path::new)
            .find_map(|b| team_of(b).map(|t| (b, t)));
        if let Some((b, team)) = signed {
            assert!(signed_by(b, &team), "{} by {team}", b.display());
            assert!(!signed_by(b, "ABCDE12345"));
        }
    }

    #[test]
    fn rollback_swaps_and_swaps_back() {
        let t = std::env::temp_dir().join(format!("rekcod-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&t).unwrap();
        let (exe, prev) = (t.join("rekcod"), previous_of(&t.join("rekcod")));
        std::fs::write(&exe, "new").unwrap();
        std::fs::write(&prev, "old").unwrap();
        swap_binaries(&exe, &prev).unwrap();
        assert_eq!((std::fs::read_to_string(&exe).unwrap(), std::fs::read_to_string(&prev).unwrap()), ("old".into(), "new".into()));
        swap_binaries(&exe, &prev).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!t.join(".rekcod.swap").exists());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn versions() {
        assert!(parse_version("0.3.0") > parse_version("0.2.9"));
        assert!(parse_version("v1.0.0") > parse_version("0.99.0"));
        assert!(compatible("0.3.1", "0.3.0") && !compatible("0.3.0", "0.2.1"));
        // Nodes and edges may lag the controller by a minor version, never lead it.
        assert!(compatible_peer("0.2.9", "0.3.0") && !compatible_peer("0.4.0", "0.3.0") && !compatible_peer("0.1.0", "0.3.0"));
        assert_eq!(listen_addr(&["x".into(), "--listen".into(), "0.0.0.0:7700".into()]).as_deref(), Some("127.0.0.1:7700"));
        assert_eq!(listen_addr(&["--listen".into(), "100.64.0.1:7700".into()]).as_deref(), Some("100.64.0.1:7700"));
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

    /// Through Paths::at, as uninstall gets it: a home rekcod created goes; a directory
    /// someone pointed REKCOD_HOME at, with their files in it, stays.
    #[test]
    fn only_a_rekcod_home_is_deleted() {
        let t = std::env::temp_dir().join(format!("rekcod-del-{}", std::process::id()));
        let made = Paths::at(t.join("made")).unwrap().home;
        std::fs::write(made.join("notes-of-mine"), "").unwrap();
        deletable(&made, Some(Path::new("/Users/me"))).unwrap();
        assert!(deletable(&made, Some(&made.join("me"))).is_err(), "the user's home is inside it");
        let docs = t.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("taxes.pdf"), "").unwrap();
        let docs = Paths::at(docs).unwrap().home;
        let e = deletable(&docs, None).unwrap_err().to_string();
        assert!(e.contains("taxes.pdf"), "{e}");
        // A 0.3.0 home: no marker, only rekcod's entries.
        let old = t.join("old");
        for d in ["run", "vm", "logs", "sb"] {
            std::fs::create_dir_all(old.join(d)).unwrap();
        }
        std::fs::write(old.join("config.json"), "{}").unwrap();
        deletable(&old, None).unwrap();
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
