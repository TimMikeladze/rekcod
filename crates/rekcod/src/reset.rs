//! `rekcod reset`: a new, empty data disk for the local Docker engine (docs/reset.md).
//!
//! Built like `uninstall`: [`facts`] reads the machine, [`plan`] turns that into
//! [`Step`]s, [`execute`] runs them with an ok/FAIL line each.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::snap::Carried;

/// One thing reset does, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Copy the snapshot volumes to the Mac ([`carry_dir`]).
    SaveSnapshots,
    /// `rekcod stop`; `switch_back` when the VM stays stopped (the docker context goes back).
    StopVm {
        switch_back: bool,
    },
    RemoveSandboxes,
    RemoveGolden(String),
    ResetDataDisk,
    Start,
    RestoreSnapshots,
}

impl Step {
    /// The ok/FAIL/skip line's text.
    fn name(&self, paths: &Paths) -> String {
        match self {
            Step::SaveSnapshots => format!("copy the snapshot volumes to {}", carry_dir(paths).display()),
            Step::StopVm { .. } => "stop the VM".into(),
            Step::RemoveSandboxes => "delete agent sandboxes".into(),
            Step::RemoveGolden(g) => format!("delete golden disk {g}"),
            Step::ResetDataDisk => format!("delete {} and mark the next one fresh", paths.data_disk().display()),
            Step::Start => "start the VM".into(),
            Step::RestoreSnapshots => "put the snapshots back".into(),
        }
    }

    /// Nothing after this one runs when it fails: the VM may still be using its disk, or
    /// the snapshots aren't safe on the Mac yet.
    fn halts(&self) -> bool {
        matches!(self, Step::SaveSnapshots | Step::StopVm { .. })
    }
}

/// What reset found before changing anything.
#[derive(Debug, Default)]
struct Facts {
    default_home: bool,
    running: bool,
    all: bool,
    no_start: bool,
    yes: bool,
    tty: bool,
    /// Snapshot volumes on the running VM's disk, and the snapshots they make up.
    snapshots: Vec<Carried>,
    snapshot_names: Vec<String>,
    /// The VM runs but its snapshots couldn't be listed.
    snapshots_unknown: bool,
    /// (id, project, bytes on the Mac); leftovers of failed creates have no project.
    sandboxes: Vec<(String, String, u64)>,
    golden: Vec<(String, u64)>,
    data_disk: u64,
    /// `name (image)` of each running container.
    containers: Vec<String>,
    /// The VM has `storage up`'s volume, which goes with the disk.
    local_storage: bool,
    /// Fleet services of this home that run, as the refusal names them.
    fleet: Vec<String>,
    /// Free bytes on the Mac's volume holding the home.
    mac_free: u64,
    /// An earlier reset deleted the disk and didn't get its snapshots back in.
    unfinished: bool,
}

impl Facts {
    fn carry(&self) -> bool {
        self.running && !self.all && !self.snapshots.is_empty()
    }

    fn carry_bytes(&self) -> u64 {
        self.snapshots.iter().map(|s| s.size).sum()
    }
}

/// Where snapshots wait on the Mac while the disk is replaced. In vm/, which Time Machine skips.
fn carry_dir(paths: &Paths) -> PathBuf {
    paths.home.join("vm/reset-snapshots")
}

/// Refusals: nothing has changed when one of these fails.
fn check(f: &Facts) -> Result<()> {
    if !f.fleet.is_empty() {
        bail!("{} on this rekcod home and owns its sandboxes; nothing changed. Stop it first, then reset", f.fleet.join(", and "));
    }
    if f.unfinished {
        return Ok(());
    }
    if !f.yes && !f.tty {
        bail!("reset deletes every image, container and volume and asks first; run it in a terminal, or pass --yes");
    }
    // The archives, plus 10% and the helper image.
    let need = f.carry_bytes() + f.carry_bytes() / 10 + (64 << 20);
    if f.carry() && need > f.mac_free {
        bail!(
            "copying the local snapshots across needs about {} on the Mac and {} is free; nothing changed. \
             Free some space, or delete snapshots you don't need (`rekcod snap rm`)",
            gib(need),
            gib(f.mac_free)
        );
    }
    Ok(())
}

fn plan(f: &Facts) -> Vec<Step> {
    use Step::*;
    if f.unfinished {
        // Deletes nothing: boots if needed and imports what the earlier reset saved.
        return if f.running { vec![RestoreSnapshots] } else { vec![Start, RestoreSnapshots] };
    }
    let carry = f.carry();
    let mut steps = Vec::new();
    if carry {
        steps.push(SaveSnapshots);
    }
    if f.running {
        steps.push(StopVm { switch_back: f.default_home && f.no_start && !carry });
    }
    if f.all {
        if !f.sandboxes.is_empty() {
            steps.push(RemoveSandboxes);
        }
        steps.extend(f.golden.iter().map(|(g, _)| RemoveGolden(g.clone())));
    }
    steps.push(ResetDataDisk);
    // The snapshots go back in through dockerd, so --no-start still boots once for them.
    if f.running && (!f.no_start || carry) {
        steps.push(Start);
    }
    if carry {
        steps.push(RestoreSnapshots);
        if f.no_start {
            steps.push(StopVm { switch_back: f.default_home });
        }
    }
    steps
}

/// Runs `steps`, printing each. A failed [`Step::halts`] step skips the rest; a failed
/// start skips putting the snapshots back. Errs naming every step that failed.
fn execute(steps: &[Step], name: impl Fn(&Step) -> String, mut run: impl FnMut(&Step) -> Result<()>) -> Result<()> {
    let (mut failed, mut halted, mut booted) = (Vec::<String>::new(), false, true);
    for step in steps {
        let name = name(step);
        if halted || (*step == Step::RestoreSnapshots && !booted) {
            println!("skip  {name}");
            continue;
        }
        match run(step) {
            Ok(()) => println!("ok    {name}"),
            Err(e) => {
                println!("FAIL  {name}: {e:#}");
                halted |= step.halts();
                booted &= *step != Step::Start;
                failed.push(name);
            }
        }
    }
    if !failed.is_empty() {
        bail!("reset is incomplete; failed: {}", failed.join("; "));
    }
    Ok(())
}

/// `512 MiB` under a GiB, else `1.2 GiB`.
fn gib(bytes: u64) -> String {
    if bytes < 1 << 30 { format!("{} MiB", bytes.div_ceil(1 << 20)) } else { format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64) }
}

/// What the prompt says, one line each.
fn summary(f: &Facts, home: &str) -> Vec<String> {
    let mut out = vec![format!("rekcod reset gives the Docker engine of {home} a new, empty data disk.")];
    out.push("Deleted:".into());
    out.push(format!("  the data disk: every image, container, volume, network and build cache ({} on the Mac)", gib(f.data_disk)));
    if !f.containers.is_empty() {
        out.push(format!("  {} running container(s): {}", f.containers.len(), f.containers.join(", ")));
    }
    let snaps = f.snapshot_names.join(", ");
    if f.all && !f.snapshot_names.is_empty() {
        out.push(format!("  local snapshots: {snaps}"));
    } else if f.all && !f.running {
        out.push("  local snapshots, if any".into());
    } else if !f.all && f.snapshots_unknown {
        out.push("  local snapshots: dockerd doesn't answer, so they can't be copied out first".into());
    } else if !f.all && !f.running {
        out.push(
            "  local snapshots, if any: the VM is stopped, so they can't be copied out first (`rekcod start` and reset again to keep them)"
                .into(),
        );
    }
    if f.local_storage {
        out.push(
            "  local S3 storage (`rekcod storage up`): its volume rekcod-storage, with every bucket and the snapshots pushed there".into(),
        );
    }
    if f.all {
        for (id, project, bytes) in &f.sandboxes {
            let what = if project.is_empty() {
                "the remains of a failed sandbox create".to_string()
            } else {
                format!("agent sandbox {id} of {project}")
            };
            out.push(format!("  {what} ({})", gib(*bytes)));
        }
        for (name, bytes) in &f.golden {
            out.push(format!("  golden disk {name} ({})", gib(*bytes)));
        }
    }
    if f.carry() {
        out.push(format!("Copied to the Mac and back onto the new disk: local snapshots {snaps} ({})", gib(f.carry_bytes())));
    }
    let mut kept = "settings (config.json), login agents, docker contexts, the socket route, logs".to_string();
    if !f.all {
        kept.push_str(", agent sandboxes, golden disks");
    }
    out.push(format!("Kept: {kept}. Snapshots pushed to S3 outside this VM (S3_ENDPOINT) aren't touched."));
    out.push(match (f.running, f.no_start) {
        (true, false) => "The VM boots again on the new disk afterwards.".into(),
        (true, true) if f.carry() => "The VM boots once to take the snapshots back, then stops (--no-start).".into(),
        (true, true) => "The VM stays stopped (--no-start); the next start formats the new disk.".into(),
        (false, _) => "The VM stays stopped; the next start formats the new disk.".into(),
    });
    out
}

/// Bytes `path` takes on disk (allocated blocks, so a sparse disk counts what it uses).
/// APFS clones share blocks, so sandboxes cloned from one disk count it each time.
fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::symlink_metadata(path) else { return 0 };
    let own = m.blocks() * 512;
    if !m.is_dir() {
        return own;
    }
    own + std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| allocated(&e.path())).sum::<u64>()
}

fn mac_free(dir: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else { return 0 };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return 0;
    }
    s.f_bavail * s.f_bsize as u64
}

/// The fleet services of this home that run: a node (its LaunchAgent/Daemon, or the lock
/// `node serve` holds) or a controller (its job, or the lock beside its database).
fn fleet_running(paths: &Paths) -> Vec<String> {
    use crate::service;
    let job = |name: &str| service::installed_here(paths, name) && service::job_running(name);
    let mut out = Vec::new();
    if job("node") || crate::paths::locked(&crate::fleet::node_lock_path(paths)) {
        out.push("a fleet node runs (`rekcod node uninstall` stops its job; a `node serve` in a terminal stops with Ctrl-C)".to_string());
    }
    let db = service::installed_here(paths, "controller")
        .then(|| service::job_args("controller"))
        .flatten()
        .and_then(|a| flag_value(&a, "--db"))
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.home.join("controller/fleet.db"));
    if job("controller") || crate::paths::locked(&db.with_extension("lock")) {
        out.push(
            "a fleet controller runs (`rekcod controller uninstall` stops its job; a `controller serve` in a terminal stops with Ctrl-C)"
                .to_string(),
        );
    }
    out
}

/// `--flag VALUE` or `--flag=VALUE` in a job's arguments.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some(v.to_string());
        }
    }
    None
}

fn facts(paths: &Paths, all: bool, no_start: bool, yes: bool) -> Facts {
    let running = crate::cli::running_pid(paths).is_some();
    let mut f = Facts {
        default_home: paths.is_default_home(),
        running,
        all,
        no_start,
        yes,
        tty: unsafe { libc::isatty(0) } == 1,
        data_disk: allocated(&paths.data_disk()),
        fleet: fleet_running(paths),
        mac_free: mac_free(&paths.home),
        unfinished: carry_dir(paths).join("manifest.json").exists(),
        ..Default::default()
    };
    if running {
        let ps: serde_json::Value =
            crate::cli::api_get(paths, "/containers/json").ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        f.containers = ps
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let name = c["Names"][0].as_str().unwrap_or("?").trim_start_matches('/');
                format!("{name} ({})", c["Image"].as_str().unwrap_or("?"))
            })
            .collect();
        f.local_storage = crate::cli::api_get(paths, "/volumes/rekcod-storage").is_ok();
        match crate::snap::carry_list(paths) {
            Ok((s, n)) => (f.snapshots, f.snapshot_names) = (s, n),
            Err(_) => f.snapshots_unknown = true,
        }
    }
    if all {
        for s in crate::sandbox::list(paths) {
            let bytes = crate::sandbox::home(paths, &s.meta.id).map(|h| allocated(&h)).unwrap_or(0);
            f.sandboxes.push((s.meta.id, s.meta.project.display().to_string(), bytes));
        }
        for h in crate::sandbox::leftovers(paths) {
            f.sandboxes.push((h.file_name().unwrap_or_default().to_string_lossy().into_owned(), String::new(), allocated(&h)));
        }
        for e in std::fs::read_dir(paths.home.join("golden")).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() && crate::sandbox::check_name("golden disk", &name).is_ok() {
                f.golden.push((name, allocated(&e.path())));
            }
        }
        f.golden.sort();
    }
    f
}

/// Every agent sandbox stopped (so its guest shuts down cleanly) and deleted, with the
/// directories failed creates left.
fn remove_sandboxes(paths: &Paths) -> Result<()> {
    for s in crate::sandbox::list(paths).into_iter().filter(|s| s.running) {
        // `rm` force-stops whatever this didn't.
        let _ = crate::sandbox::stop(paths, &s.meta.id);
    }
    crate::sandbox::rm(paths, &[], true)?;
    for h in crate::sandbox::leftovers(paths) {
        crate::sandbox::remove_leftover(&h)?;
    }
    Ok(())
}

/// Deletes data.img and writes data.fresh, which is what a first install leaves before its
/// first boot: the guest formats the new blank disk (rekcod.fresh=1). Holds the boot lock,
/// so nothing boots onto the disk meanwhile; the disk goes before the marker is written, so
/// an old disk never has one beside it.
fn reset_data_disk(paths: &Paths) -> Result<()> {
    let _lock = crate::paths::lock(&paths.boot_lock())?;
    if let Some(pid) = crate::cli::running_pid(paths) {
        bail!("the VM was started again (pid {pid}); its disk stays");
    }
    match std::fs::remove_file(paths.data_disk()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("delete {}", paths.data_disk().display()));
        }
        _ => {}
    }
    std::fs::write(paths.data_fresh(), b"").with_context(|| format!("write {}", paths.data_fresh().display()))
}

/// `rekcod start --no-switch` with the saved settings: the docker context and socket route
/// stay as they are, and config.json isn't changed (the disk size passed is the saved one).
fn start(paths: &Paths) -> Result<()> {
    let disk = crate::cli::saved(paths).disk_gib.unwrap_or(crate::paths::DEFAULT_DISK_GIB);
    crate::cli::start(paths, None, (None, None), disk, true, false, None)
}

/// `rekcod reset [--all] [--no-start] [--yes]`.
pub fn reset(paths: &Paths, all: bool, no_start: bool, yes: bool) -> Result<()> {
    let f = facts(paths, all, no_start, yes);
    let home = paths.home.display().to_string();
    check(&f)?;
    if f.unfinished {
        println!("An earlier reset of {home} didn't put its snapshots back; importing them now. Nothing is deleted.");
    } else {
        for line in summary(&f, &home) {
            println!("{line}");
        }
        if !yes {
            eprint!("Type `yes` to go on: ");
            std::io::stderr().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if line.trim() != "yes" {
                bail!("nothing changed");
            }
        }
    }
    let (mut free_before, mut free_after, mut disk_gone) = (None, None, false);
    let carry = carry_dir(paths);
    let r = execute(
        &plan(&f),
        |s| s.name(paths),
        |step| match step {
            Step::SaveSnapshots => {
                let r = crate::snap::carry_out(paths, &carry, &f.snapshots);
                if r.is_err() {
                    let _ = std::fs::remove_dir_all(&carry);
                }
                r
            }
            Step::StopVm { switch_back } => crate::cli::stop(paths, !switch_back, false),
            Step::RemoveSandboxes => {
                free_before.get_or_insert_with(|| mac_free(&paths.home));
                remove_sandboxes(paths)
            }
            Step::RemoveGolden(g) => {
                free_before.get_or_insert_with(|| mac_free(&paths.home));
                crate::sandbox::golden_rm(paths, g)
            }
            Step::ResetDataDisk => {
                free_before.get_or_insert_with(|| mac_free(&paths.home));
                let r = reset_data_disk(paths);
                disk_gone = !paths.data_disk().exists();
                free_after = Some(mac_free(&paths.home));
                r
            }
            Step::Start => start(paths),
            Step::RestoreSnapshots => crate::snap::carry_in(paths, &carry).map(|n| println!("      {n} snapshot volume(s) back")),
        },
    );
    if let (Some(b), Some(a)) = (free_before, free_after) {
        println!("{} back on the Mac", gib(a.saturating_sub(b)));
    }
    if carry.exists() {
        if f.unfinished || disk_gone {
            println!("The snapshots are still in {}; `rekcod reset` again puts them back once the VM boots.", carry.display());
        } else {
            // The disk wasn't deleted, so the snapshots are still on it.
            let _ = std::fs::remove_dir_all(&carry);
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use Step::*;

    fn snap(name: &str) -> Carried {
        Carried { name: name.into(), size: 1 << 20, ..Default::default() }
    }

    fn facts(running: bool, all: bool, no_start: bool) -> Facts {
        Facts {
            default_home: true,
            running,
            all,
            no_start,
            yes: true,
            snapshots: if running { vec![snap("rekcod-snap.shop.seed.1.db")] } else { vec![] },
            sandboxes: vec![("ab12cd34".into(), "/src/shop".into(), 1 << 30)],
            golden: vec![("default".into(), 1 << 30)],
            mac_free: 100 << 30,
            ..Default::default()
        }
    }

    #[test]
    fn reset_plans() {
        // Default: snapshots ride along, sandboxes and golden disks stay, the VM comes back.
        assert_eq!(
            plan(&facts(true, false, false)),
            [SaveSnapshots, StopVm { switch_back: false }, ResetDataDisk, Start, RestoreSnapshots]
        );
        // --all deletes the snapshots with the disk, and the sandboxes and golden disks.
        assert_eq!(
            plan(&facts(true, true, false)),
            [StopVm { switch_back: false }, RemoveSandboxes, RemoveGolden("default".into()), ResetDataDisk, Start]
        );
        // A stopped VM stays stopped; nothing to copy out.
        assert_eq!(plan(&facts(false, false, false)), [ResetDataDisk]);
        assert_eq!(plan(&facts(false, true, true)), [RemoveSandboxes, RemoveGolden("default".into()), ResetDataDisk]);
        // --no-start: stopped like `rekcod stop` (context back) ...
        assert_eq!(
            plan(&facts(true, true, true)),
            [StopVm { switch_back: true }, RemoveSandboxes, RemoveGolden("default".into()), ResetDataDisk]
        );
        // ... except that snapshots need one boot to go back in.
        assert_eq!(
            plan(&facts(true, false, true)),
            [SaveSnapshots, StopVm { switch_back: false }, ResetDataDisk, Start, RestoreSnapshots, StopVm { switch_back: true }]
        );
        // Another REKCOD_HOME never switches the shared docker context.
        let other = Facts { default_home: false, ..facts(true, true, true) };
        assert!(plan(&other).iter().all(|s| !matches!(s, StopVm { switch_back: true })));
        // An unfinished reset only puts its snapshots back.
        assert_eq!(plan(&Facts { unfinished: true, ..facts(false, true, false) }), [Start, RestoreSnapshots]);
        assert_eq!(plan(&Facts { unfinished: true, ..facts(true, false, false) }), [RestoreSnapshots]);
    }

    #[test]
    fn refuses_before_changing_anything() {
        let fleet = Facts { fleet: vec!["a fleet node runs".into()], ..facts(true, false, false) };
        assert!(check(&fleet).unwrap_err().to_string().contains("fleet node"));
        let piped = Facts { yes: false, tty: false, ..facts(true, false, false) };
        assert!(check(&piped).unwrap_err().to_string().contains("--yes"));
        assert!(check(&Facts { tty: true, ..piped }).is_ok());
        let full = Facts { mac_free: 1 << 20, ..facts(true, false, false) };
        assert!(check(&full).unwrap_err().to_string().contains("snap rm"));
        // --all doesn't copy them, so it needs no room.
        assert!(check(&Facts { mac_free: 1 << 20, ..facts(true, true, false) }).is_ok());
    }

    /// A failed stop deletes nothing; a failed boot leaves the snapshots on the Mac.
    #[test]
    fn a_failed_stop_halts_and_a_failed_boot_keeps_the_snapshots() {
        let steps = plan(&facts(true, true, false));
        let mut ran = Vec::new();
        let r = execute(
            &steps,
            |s| format!("{s:?}"),
            |s| {
                ran.push(s.clone());
                if matches!(s, StopVm { .. }) { Err(anyhow::anyhow!("daemon did not exit")) } else { Ok(()) }
            },
        );
        assert!(r.is_err());
        assert_eq!(ran, [StopVm { switch_back: false }]);

        let mut ran = Vec::new();
        let r = execute(
            &plan(&facts(true, false, false)),
            |s| format!("{s:?}"),
            |s| {
                ran.push(s.clone());
                if *s == Start { Err(anyhow::anyhow!("dockerd did not answer")) } else { Ok(()) }
            },
        );
        assert!(r.unwrap_err().to_string().contains("Start"));
        assert!(!ran.contains(&RestoreSnapshots) && ran.contains(&ResetDataDisk));
    }

    /// On a scratch home: the disk goes, data.fresh is written, and everything else stays.
    #[test]
    fn data_disk_reset_keeps_the_rest_of_the_home() {
        let t = std::env::temp_dir().join(format!("rekcod-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let paths = Paths::at(t.clone()).unwrap();
        let keep = [
            "config.json",
            "vm/root.img",
            "vm/mac-address",
            "vm/guest-version",
            "vm/warm-paths",
            "sb/ab12cd34/meta.json",
            "golden/default/vm/data.img",
        ];
        for k in keep {
            std::fs::create_dir_all(t.join(k).parent().unwrap()).unwrap();
            std::fs::write(t.join(k), k).unwrap();
        }
        std::fs::write(paths.data_disk(), b"ext4").unwrap();
        let f = Facts { yes: true, ..facts(false, false, false) };
        let r = execute(
            &plan(&f),
            |s| format!("{s:?}"),
            |s| {
                assert_eq!(*s, ResetDataDisk);
                reset_data_disk(&paths)
            },
        );
        r.unwrap();
        assert!(!paths.data_disk().exists() && paths.data_fresh().exists());
        for k in keep {
            assert_eq!(std::fs::read_to_string(t.join(k)).unwrap(), k, "{k}");
        }
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn the_prompt_names_what_goes() {
        let f = Facts { containers: vec!["web (nginx)".into()], snapshot_names: vec!["shop/seed".into()], ..facts(true, false, false) };
        let s = summary(&f, "/h").join("\n");
        assert!(
            s.contains("1 running container(s): web (nginx)") && s.contains("back onto the new disk: local snapshots shop/seed"),
            "{s}"
        );
        assert!(!s.contains("ab12cd34") && s.contains("agent sandboxes, golden disks"), "{s}");
        let s = summary(&Facts { snapshot_names: vec!["shop/seed".into()], ..facts(true, true, false) }, "/h").join("\n");
        assert!(
            s.contains("agent sandbox ab12cd34 of /src/shop")
                && s.contains("golden disk default")
                && s.contains("local snapshots: shop/seed"),
            "{s}"
        );
        assert!(summary(&facts(false, false, false), "/h").join("\n").contains("can't be copied out first"));
        assert!(summary(&facts(false, true, false), "/h").join("\n").contains("local snapshots, if any"));
        assert!(summary(&Facts { local_storage: true, ..facts(true, false, false) }, "/h").join("\n").contains("rekcod-storage"));
        assert_eq!(flag_value(&["serve".into(), "--db=/x/f.db".into()], "--db").as_deref(), Some("/x/f.db"));
        assert_eq!(flag_value(&["--db".into(), "/y".into()], "--db").as_deref(), Some("/y"));
    }
}
