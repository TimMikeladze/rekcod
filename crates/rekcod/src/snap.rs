//! `rekcod snap`: save and restore a compose project's named volumes
//! (docs/phase5.md §5, plan.md §3a).
//!
//! A snapshot of volume `v` is another volume, `rekcod-snap.<project>.<name>.<v>`,
//! labelled so it can be found again. Save pauses the project's running
//! containers for a crash-consistent copy (databases recover through their
//! WAL, as after a power cut). Restore stops them, copies back, and starts the
//! same containers again. Copies run in one helper container with `cp -a`.
//!
//! Each save writes a new generation: volumes named
//! `rekcod-snap.<project>.<name>.<gen>.<volume>`, then an empty marker volume
//! (`dev.rekcod.snap.done`) once every copy succeeded, and only then deletes the previous
//! generation. A save that fails or is killed leaves the previous snapshot as it was.
//! Restore first saves the current volumes as `pre-restore`.
//!
//! `save --push` also uploads each snapshot volume as `snaps/<project>/<name>/<volume>.tar.zst`
//! with a `manifest.json`; `restore --pull` recreates the snapshot from them first, so it
//! comes back on another Mac (docs/rustfs.md).

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::paths::Paths;

const HELPER_IMAGE: &str = "alpine:3.22";
const SNAP: &str = "dev.rekcod.snap";
const SNAP_PROJECT: &str = "dev.rekcod.snap.project";
const SNAP_NAME: &str = "dev.rekcod.snap.name";
const SNAP_VOLUME: &str = "dev.rekcod.snap.volume";
const SNAP_CREATED: &str = "dev.rekcod.snap.created";
/// The source volume's own labels (JSON), so a restore after `down -v`
/// recreates volumes that compose will adopt.
const SNAP_LABELS: &str = "dev.rekcod.snap.labels";
/// Which save wrote a volume. Snapshots from before generations have none.
const SNAP_GEN: &str = "dev.rekcod.snap.gen";
/// On the marker volume that says a generation is complete.
const SNAP_DONE: &str = "dev.rekcod.snap.done";
/// The name as given (a branch like `feature/x`), before it was made volume-safe.
const SNAP_RAW: &str = "dev.rekcod.snap.raw";
/// What `restore` saves the current volumes as before replacing them.
pub const PRE_RESTORE: &str = "pre-restore";
const PROJECT_LABEL: &str = "com.docker.compose.project";
const WORKDIR_LABEL: &str = "com.docker.compose.project.working_dir";

/// `docker -H <rekcod> <args>`; stdout on success.
fn docker(paths: &Paths, args: &[&str]) -> Result<String> {
    let mut cmd = crate::docker::any_cli(paths)?;
    let out = cmd.args(["-H", &paths.docker_host()]).args(args).stdin(std::process::Stdio::null()).output()?;
    if !out.status.success() {
        bail!("docker {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn lines(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// Compose's project-name rule: lowercase, `[a-z0-9_-]`, starting with a letter or digit.
pub fn normalize_project(name: &str) -> String {
    let s: String = name.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
    s.trim_start_matches(['_', '-']).to_string()
}

/// A snapshot name usable inside a volume name (git branches have `/`).
pub fn normalize_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') { c } else { '-' }).collect();
    s.trim_matches('-').to_string()
}

fn snapshot_volume(project: &str, name: &str, gen_: &str, volume: &str) -> String {
    format!("rekcod-snap.{project}.{name}.{gen_}.{volume}")
}

fn marker_volume(project: &str, name: &str, gen_: &str) -> String {
    format!("rekcod-snap.{project}.{name}.{gen_}.done")
}

/// `-p`, then `COMPOSE_PROJECT_NAME`, then the project whose containers were
/// started from this directory, then the directory name.
fn project(paths: &Paths, explicit: Option<&str>) -> Result<String> {
    if let Some(p) = explicit.map(str::to_string).or_else(|| std::env::var("COMPOSE_PROJECT_NAME").ok()).filter(|p| !p.is_empty()) {
        return Ok(normalize_project(&p));
    }
    let cwd = std::env::current_dir()?;
    let filter = format!("label={WORKDIR_LABEL}={}", cwd.display());
    let fmt = format!("{{{{.Label \"{PROJECT_LABEL}\"}}}}");
    if let Some(p) = lines(&docker(paths, &["ps", "-a", "--filter", &filter, "--format", &fmt])?).into_iter().next() {
        return Ok(p);
    }
    let dir = cwd.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let p = normalize_project(dir);
    if p.is_empty() {
        bail!("can't tell the compose project here; pass -p <project>");
    }
    Ok(p)
}

fn git_branch() -> Option<String> {
    // symbolic-ref also works before the first commit; detached HEAD fails.
    let out = std::process::Command::new("git").args(["symbolic-ref", "--short", "-q", "HEAD"]).output().ok()?;
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !b.is_empty() && b != "HEAD").then_some(b)
}

/// `name`, else the git branch. Saving outside git falls back to a timestamp,
/// so the menu bar can always save.
fn snap_name(name: Option<&str>, saving: bool) -> Result<String> {
    let raw = match (name, git_branch()) {
        (Some(n), _) => n.to_string(),
        (None, Some(branch)) => branch,
        (None, None) if saving => format!("snap-{}", now()),
        (None, None) => bail!("no snapshot name given and not on a git branch"),
    };
    let n = normalize_name(&raw);
    if n.is_empty() {
        bail!("invalid snapshot name {raw:?}");
    }
    Ok(n)
}

fn project_volumes(paths: &Paths, project: &str) -> Result<Vec<String>> {
    let filter = format!("label={PROJECT_LABEL}={project}");
    Ok(lines(&docker(paths, &["volume", "ls", "--filter", &filter, "--format", "{{.Name}}"])?))
}

fn running_containers(paths: &Paths, project: &str) -> Result<Vec<String>> {
    let filter = format!("label={PROJECT_LABEL}={project}");
    Ok(lines(&docker(paths, &["ps", "--filter", &filter, "--filter", "status=running", "--format", "{{.ID}}"])?))
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Snapshot {
    pub project: String,
    pub name: String,
    pub created: u64,
    /// Project volume -> snapshot volume.
    pub volumes: BTreeMap<String, String>,
    /// Every volume of this (project, name) that isn't the live generation: older
    /// generations, markers, and saves that never finished. Safe to delete.
    #[serde(skip)]
    pub stale: Vec<String>,
    /// The name as given, when it differs from `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

#[derive(Default)]
struct Generation {
    created: u64,
    done: bool,
    raw: Option<String>,
    volumes: BTreeMap<String, String>,
    markers: Vec<String>,
}

/// Snapshots from `GET /volumes`: for each (project, name) the newest complete generation.
pub fn parse(json: &[u8]) -> Vec<Snapshot> {
    let v: serde_json::Value = serde_json::from_slice(json).unwrap_or_default();
    let mut by_key: BTreeMap<(String, String), BTreeMap<String, Generation>> = BTreeMap::new();
    for vol in v["Volumes"].as_array().into_iter().flatten() {
        let l = &vol["Labels"];
        let (Some(vname), Some(project), Some(name)) = (vol["Name"].as_str(), l[SNAP_PROJECT].as_str(), l[SNAP_NAME].as_str()) else {
            continue;
        };
        let gen_ = l[SNAP_GEN].as_str().unwrap_or("");
        let g = by_key.entry((project.into(), name.into())).or_default().entry(gen_.into()).or_default();
        g.created = g.created.max(l[SNAP_CREATED].as_str().and_then(|c| c.parse().ok()).unwrap_or(0));
        // Legacy snapshots (no generation) were always written in one go.
        g.done |= gen_.is_empty();
        if l[SNAP_DONE].as_str().is_some() {
            g.done = true;
            g.markers.push(vname.into());
        } else if let Some(source) = l[SNAP_VOLUME].as_str() {
            g.volumes.insert(source.into(), vname.into());
        }
        if let Some(raw) = l[SNAP_RAW].as_str().filter(|r| *r != name) {
            g.raw = Some(raw.into());
        }
    }
    let mut out = Vec::new();
    for ((project, name), gens) in by_key {
        let live = gens.iter().filter(|(_, g)| g.done && !g.volumes.is_empty()).max_by_key(|(_, g)| g.created).map(|(k, _)| k.clone());
        let mut stale = Vec::new();
        for (k, g) in &gens {
            if Some(k) != live.as_ref() {
                stale.extend(g.volumes.values().cloned());
                stale.extend(g.markers.iter().cloned());
            }
        }
        if let Some(g) = live.and_then(|k| gens.into_iter().find(|(gk, _)| *gk == k).map(|(_, g)| g)) {
            out.push(Snapshot { project, name, created: g.created, volumes: g.volumes, stale, raw: g.raw });
        } else if !stale.is_empty() {
            // Only unfinished saves: nothing to restore, but `rm` can clean them up.
            out.push(Snapshot { project, name, created: 0, volumes: BTreeMap::new(), stale, raw: None });
        }
    }
    out
}

/// Snapshots on the VM, restorable ones only, optionally for one project.
fn snapshots(paths: &Paths, project: Option<&str>) -> Result<Vec<Snapshot>> {
    Ok(all_snapshots(paths)?.into_iter().filter(|s| !s.volumes.is_empty() && project.is_none_or(|p| s.project == p)).collect())
}

fn all_snapshots(paths: &Paths) -> Result<Vec<Snapshot>> {
    let filter = "%7B%22label%22%3A%5B%22dev.rekcod.snap%22%5D%7D"; // {"label":["dev.rekcod.snap"]}
    Ok(parse(&crate::cli::api_get(paths, &format!("/volumes?filters={filter}"))?))
}

fn ensure_helper(paths: &Paths) -> Result<()> {
    if docker(paths, &["image", "inspect", HELPER_IMAGE]).is_err() {
        docker(paths, &["pull", "-q", HELPER_IMAGE])?;
    }
    Ok(())
}

/// Replaces each `to` volume's contents with its `from` volume's, in one helper container.
fn copy_volumes(paths: &Paths, pairs: &[(String, String)]) -> Result<()> {
    ensure_helper(paths)?;
    let mut args: Vec<String> = vec!["run".into(), "--rm".into(), "--network".into(), "none".into()];
    let mut script = String::from("set -e");
    for (i, (from, to)) in pairs.iter().enumerate() {
        args.extend(["-v".into(), format!("{from}:/from/{i}:ro"), "-v".into(), format!("{to}:/to/{i}")]);
        script.push_str(&format!("; find /to/{i} -mindepth 1 -delete; cp -a /from/{i}/. /to/{i}/"));
    }
    args.extend([HELPER_IMAGE.into(), "sh".into(), "-c".into(), script]);
    docker(paths, &args.iter().map(String::as_str).collect::<Vec<_>>()).map(|_| ())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn snap_prefix(project: &str, name: &str) -> String {
    format!("snaps/{project}/{name}")
}

/// Uploads each snapshot volume (tar, zstd) and a manifest with the source volumes' labels.
fn push(paths: &Paths, project: &str, name: &str, pairs: &[(String, String)]) -> Result<()> {
    let (s3, bucket) = crate::storage::snap_target(paths)?;
    let prefix = snap_prefix(project, name);
    let mut manifest = serde_json::Map::new();
    for (v, snap) in pairs {
        let tmp = paths.home.join(format!("snap-push-{}.tar.zst", std::process::id()));
        let r = (|| -> Result<()> {
            let mut cmd = crate::docker::any_cli(paths)?;
            let mut child = cmd
                .args(["-H", &paths.docker_host(), "run", "--rm", "--network", "none", "-v", &format!("{snap}:/v:ro"), HELPER_IMAGE])
                .args(["tar", "-C", "/v", "-cf", "-", "."])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let mut enc = zstd::Encoder::new(std::fs::File::create(&tmp)?, 3)?;
            std::io::copy(child.stdout.as_mut().context("no stdout")?, &mut enc)?;
            enc.finish()?;
            if !child.wait()?.success() {
                bail!("tar of {snap} failed");
            }
            s3.put(&bucket, &format!("{prefix}/{v}.tar.zst"), crate::s3::Body::File(&tmp))
        })();
        let _ = std::fs::remove_file(&tmp);
        r?;
        let labels = docker(paths, &["volume", "inspect", "--format", "{{json .Labels}}", v])?;
        manifest.insert(v.clone(), serde_json::from_str(&labels).unwrap_or(serde_json::Value::Null));
    }
    let m = serde_json::json!({"project": project, "name": name, "created": now(), "volumes": manifest});
    s3.put(&bucket, &format!("{prefix}/manifest.json"), crate::s3::Body::Bytes(m.to_string().as_bytes()))?;
    println!("pushed `{name}` to {}/{bucket}/{prefix}", s3.endpoint);
    Ok(())
}

/// Recreates snapshot `name` of `project` from what `push` uploaded, replacing a local one.
fn pull(paths: &Paths, project: &str, name: &str) -> Result<()> {
    let (s3, bucket) = crate::storage::snap_target(paths)?;
    let prefix = snap_prefix(project, name);
    let mut b = Vec::new();
    s3.get(&bucket, &format!("{prefix}/manifest.json"), &mut b).with_context(|| format!("no pushed snapshot {prefix}"))?;
    let m: serde_json::Value = serde_json::from_slice(&b)?;
    let created = m["created"].as_u64().unwrap_or_else(now);
    ensure_helper(paths)?;
    let gen_ = new_gen(created)?;
    let mut made = Vec::new();
    for (v, labels) in m["volumes"].as_object().into_iter().flatten() {
        let snap = create_snap_volume(paths, project, name, None, &gen_, created, v, &labels.to_string())?;
        made.push(snap.clone());
        let tmp = paths.home.join(format!("snap-pull-{}.tar.zst", std::process::id()));
        let r = (|| -> Result<()> {
            s3.get(&bucket, &format!("{prefix}/{v}.tar.zst"), &mut std::fs::File::create(&tmp)?)?;
            let mut cmd = crate::docker::any_cli(paths)?;
            let mut child = cmd
                .args(["-H", &paths.docker_host(), "run", "--rm", "-i", "--network", "none", "-v", &format!("{snap}:/v"), HELPER_IMAGE])
                .args(["tar", "-C", "/v", "-xf", "-"])
                .stdin(std::process::Stdio::piped())
                .spawn()?;
            let mut stdin = child.stdin.take().context("no stdin")?;
            zstd::stream::copy_decode(std::fs::File::open(&tmp)?, &mut stdin)?;
            drop(stdin);
            if !child.wait()?.success() {
                bail!("untar into {snap} failed");
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = r {
            remove_volumes(paths, &made);
            return Err(e);
        }
    }
    finish(paths, project, name, &gen_, created)?;
    eprintln!("rekcod: pulled `{name}` for {project} from {}/{bucket}/{prefix}", s3.endpoint);
    Ok(())
}

/// A generation id: the save's time plus a few random letters.
fn new_gen(created: u64) -> Result<String> {
    Ok(format!("{created}{}", &crate::sandbox::new_id()?[..4]))
}

#[allow(clippy::too_many_arguments)]
fn create_snap_volume(
    paths: &Paths,
    project: &str,
    name: &str,
    raw: Option<&str>,
    gen_: &str,
    created: u64,
    source: &str,
    labels: &str,
) -> Result<String> {
    let snap = snapshot_volume(project, name, gen_, source);
    let mut args: Vec<String> = ["volume", "create"].map(String::from).into();
    for (k, v) in [
        (SNAP, "1"),
        (SNAP_PROJECT, project),
        (SNAP_NAME, name),
        (SNAP_VOLUME, source),
        (SNAP_GEN, gen_),
        (SNAP_CREATED, &created.to_string()),
        (SNAP_LABELS, labels),
    ] {
        args.extend(["--label".into(), format!("{k}={v}")]);
    }
    if let Some(r) = raw.filter(|r| *r != name) {
        args.extend(["--label".into(), format!("{SNAP_RAW}={r}")]);
    }
    args.push(snap.clone());
    docker(paths, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(snap)
}

fn remove_volumes(paths: &Paths, vols: &[String]) {
    if !vols.is_empty() {
        let _ = docker(paths, &[&["volume", "rm", "-f"][..], &vols.iter().map(String::as_str).collect::<Vec<_>>()].concat());
    }
}

/// Marks generation `gen_` complete, then deletes every other generation of the snapshot.
fn finish(paths: &Paths, project: &str, name: &str, gen_: &str, created: u64) -> Result<()> {
    docker(
        paths,
        &[
            "volume",
            "create",
            "--label",
            &format!("{SNAP}=1"),
            "--label",
            &format!("{SNAP_PROJECT}={project}"),
            "--label",
            &format!("{SNAP_NAME}={name}"),
            "--label",
            &format!("{SNAP_GEN}={gen_}"),
            "--label",
            &format!("{SNAP_CREATED}={created}"),
            "--label",
            &format!("{SNAP_DONE}=1"),
            &marker_volume(project, name, gen_),
        ],
    )?;
    if let Some(s) = all_snapshots(paths)?.into_iter().find(|s| s.project == project && s.name == name) {
        remove_volumes(paths, &s.stale);
    }
    Ok(())
}

/// Ignores Ctrl-C, SIGTERM and SIGHUP while alive, so the project's containers are never
/// left paused by an interrupted save. Child processes still get the signal, which fails
/// the copy; the containers are then unpaused as usual.
struct HoldSignals([libc::sighandler_t; 3]);

const HELD: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

impl HoldSignals {
    fn new() -> HoldSignals {
        HoldSignals(HELD.map(|sig| unsafe { libc::signal(sig, libc::SIG_IGN) }))
    }
}

impl Drop for HoldSignals {
    fn drop(&mut self) {
        for (sig, old) in HELD.iter().zip(self.0) {
            unsafe { libc::signal(*sig, old) };
        }
    }
}

pub fn save(paths: &Paths, explicit_project: Option<&str>, name: Option<&str>, push_it: bool) -> Result<()> {
    let project = project(paths, explicit_project)?;
    let raw = match name {
        Some(n) => n.to_string(),
        None => git_branch().unwrap_or_default(),
    };
    let name = snap_name(name, true)?;
    if let Some(other) = all_snapshots(paths)?.iter().find(|s| s.project == project && s.name == name)
        && other.raw.as_deref().unwrap_or(&other.name) != raw.as_str()
        && !raw.is_empty()
    {
        bail!(
            "`{raw}` and `{}` both become snapshot `{name}`; pass a name: rekcod snap save <name>",
            other.raw.as_deref().unwrap_or(&other.name)
        );
    }
    save_as(paths, &project, &name, Some(&raw), push_it)
}

fn save_as(paths: &Paths, project: &str, name: &str, raw: Option<&str>, push_it: bool) -> Result<()> {
    let t0 = Instant::now();
    let volumes = project_volumes(paths, project)?;
    if volumes.is_empty() {
        bail!("compose project `{project}` has no named volumes to snapshot (-p to pick another project)");
    }
    let created = now();
    let gen_ = new_gen(created)?;
    let mut pairs = Vec::new();
    let made = |pairs: &Vec<(String, String)>| pairs.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();
    for v in &volumes {
        let labels = docker(paths, &["volume", "inspect", "--format", "{{json .Labels}}", v])?;
        match create_snap_volume(paths, project, name, raw, &gen_, created, v, &labels) {
            Ok(snap) => pairs.push((v.clone(), snap)),
            Err(e) => {
                remove_volumes(paths, &made(&pairs));
                return Err(e);
            }
        }
    }
    let running = running_containers(paths, project)?;
    let copied = {
        let _held = HoldSignals::new();
        if !running.is_empty() {
            docker(paths, &[&["pause"][..], &running.iter().map(String::as_str).collect::<Vec<_>>()].concat())?;
        }
        let copied = copy_volumes(paths, &pairs);
        if !running.is_empty() {
            let unpaused = docker(paths, &[&["unpause"][..], &running.iter().map(String::as_str).collect::<Vec<_>>()].concat());
            if let Err(e) = unpaused {
                eprintln!("rekcod: warning: {e:#}");
            }
        }
        copied
    };
    if let Err(e) = copied {
        // The previous snapshot of this name is untouched.
        remove_volumes(paths, &made(&pairs));
        return Err(e);
    }
    finish(paths, project, name, &gen_, created)?;
    println!(
        "saved `{name}` for {project}: {} volume(s), {} container(s) paused, {:.1}s",
        pairs.len(),
        running.len(),
        t0.elapsed().as_secs_f64()
    );
    if push_it {
        push(paths, project, name, &pairs)?;
    }
    Ok(())
}

pub fn restore(paths: &Paths, explicit_project: Option<&str>, name: Option<&str>, pull_it: bool) -> Result<()> {
    let t0 = Instant::now();
    let project = project(paths, explicit_project)?;
    let name = snap_name(name, false)?;
    if pull_it {
        pull(paths, &project, &name)?;
    }
    let all = snapshots(paths, Some(&project))?;
    let Some(snap) = all.iter().find(|s| s.name == name) else {
        let have: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        bail!("no snapshot `{name}` for {project} (have: {})", if have.is_empty() { "none".into() } else { have.join(", ") });
    };
    for (v, s) in &snap.volumes {
        if docker(paths, &["volume", "inspect", v]).is_err() {
            // Gone after `down -v`: recreate it with the labels compose expects.
            let fmt = format!("{{{{index .Labels \"{SNAP_LABELS}\"}}}}");
            let labels: BTreeMap<String, String> =
                serde_json::from_str(&docker(paths, &["volume", "inspect", "--format", &fmt, s])?).ok().flatten().unwrap_or_default();
            let mut args: Vec<String> = vec!["volume".into(), "create".into()];
            for (k, val) in labels {
                args.extend(["--label".into(), format!("{k}={val}")]);
            }
            args.push(v.clone());
            docker(paths, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
    }
    let running = running_containers(paths, &project)?;
    if !running.is_empty() {
        docker(paths, &[&["stop"][..], &running.iter().map(String::as_str).collect::<Vec<_>>()].concat())?;
    }
    // What's there now, so a restore to the wrong snapshot can be undone.
    let backed_up = if name != PRE_RESTORE { save_as(paths, &project, PRE_RESTORE, None, false) } else { Ok(()) };
    let pairs: Vec<(String, String)> = snap.volumes.iter().map(|(v, s)| (s.clone(), v.clone())).collect();
    let copied = backed_up.and_then(|_| copy_volumes(paths, &pairs));
    if !running.is_empty() {
        let started = docker(paths, &[&["start"][..], &running.iter().map(String::as_str).collect::<Vec<_>>()].concat());
        if let Err(e) = started {
            eprintln!("rekcod: warning: {e:#}");
        }
    }
    copied?;
    println!(
        "restored `{name}` for {project}: {} volume(s), {} container(s) restarted, {:.1}s{}",
        pairs.len(),
        running.len(),
        t0.elapsed().as_secs_f64(),
        if name != PRE_RESTORE { format!("; the previous state is snapshot `{PRE_RESTORE}`") } else { String::new() }
    );
    Ok(())
}

pub fn list(paths: &Paths, explicit_project: Option<&str>, all: bool, json: bool) -> Result<()> {
    let project = if all { None } else { Some(project(paths, explicit_project)?) };
    let snaps = snapshots(paths, project.as_deref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snaps)?);
        return Ok(());
    }
    if snaps.is_empty() {
        println!("no snapshots{}", project.map(|p| format!(" for {p}")).unwrap_or_default());
        return Ok(());
    }
    let now = now();
    for s in snaps {
        let age = now.saturating_sub(s.created);
        let age = match age {
            a if a < 120 => format!("{a}s ago"),
            a if a < 7200 => format!("{}m ago", a / 60),
            a if a < 172_800 => format!("{}h ago", a / 3600),
            a => format!("{}d ago", a / 86_400),
        };
        let vols: Vec<&str> = s.volumes.keys().map(String::as_str).collect();
        println!("{:<24} {:<16} {:<10} {}", s.name, s.project, age, vols.join(", "));
    }
    Ok(())
}

pub fn remove(paths: &Paths, explicit_project: Option<&str>, name: &str) -> Result<()> {
    let project = project(paths, explicit_project)?;
    let name = normalize_name(name);
    let snaps: Vec<Snapshot> = all_snapshots(paths)?.into_iter().filter(|s| s.project == project).collect();
    let Some(snap) = snaps.iter().find(|s| s.name == name) else { bail!("no snapshot `{name}` for {project}") };
    let mut vols: Vec<&str> = snap.volumes.values().map(String::as_str).collect();
    vols.extend(snap.stale.iter().map(String::as_str));
    docker(paths, &[&["volume", "rm"][..], &vols].concat())?;
    println!("removed `{name}` for {project}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_compose_and_volume_rules() {
        assert_eq!(normalize_project("My App.v2"), "myappv2");
        assert_eq!(normalize_project("_x-Y"), "x-y");
        assert_eq!(normalize_name("feature/login-2"), "feature-login-2");
        assert_eq!(normalize_name("//"), "");
        assert_eq!(snapshot_volume("shop", "seeded", "17abcd", "shop_pg"), "rekcod-snap.shop.seeded.17abcd.shop_pg");
    }

    #[test]
    fn the_newest_complete_generation_wins() {
        let vol = |name: &str, gen_: Option<&str>, created: u64, extra: serde_json::Value| {
            let mut l = serde_json::json!({SNAP: "1", SNAP_PROJECT: "shop", SNAP_NAME: "seeded", SNAP_CREATED: created.to_string()});
            if let Some(g) = gen_ {
                l[SNAP_GEN] = g.into();
            }
            for (k, v) in extra.as_object().unwrap() {
                l[k] = v.clone();
            }
            serde_json::json!({"Name": name, "Labels": l})
        };
        let json = serde_json::json!({"Volumes": [
            // A legacy snapshot, then a finished generation, then a save that never finished.
            vol("rekcod-snap.shop.seeded.shop_pg", None, 10, serde_json::json!({SNAP_VOLUME: "shop_pg"})),
            vol("g20.pg", Some("20a"), 20, serde_json::json!({SNAP_VOLUME: "shop_pg"})),
            vol("g20.done", Some("20a"), 20, serde_json::json!({SNAP_DONE: "1"})),
            vol("g30.pg", Some("30b"), 30, serde_json::json!({SNAP_VOLUME: "shop_pg", SNAP_RAW: "feature/x"})),
        ]});
        let s = parse(json.to_string().as_bytes());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].created, 20);
        assert_eq!(s[0].volumes["shop_pg"], "g20.pg");
        let mut stale = s[0].stale.clone();
        stale.sort();
        assert_eq!(stale, ["g30.pg", "rekcod-snap.shop.seeded.shop_pg"]);
    }
}
