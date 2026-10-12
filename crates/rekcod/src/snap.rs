//! `rekcod snap`: save and restore a compose project's named volumes
//! (docs/phase5.md §5, plan.md §3a).
//!
//! A snapshot of volume `v` is another volume, `rekcod-snap.<project>.<name>.<v>`,
//! labelled so it can be found again. Save pauses the project's running
//! containers for a crash-consistent copy (databases recover through their
//! WAL, as after a power cut). Restore stops them, copies back, and starts the
//! same containers again. Copies run in one helper container with `cp -a`.

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
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

pub fn snapshot_volume(project: &str, name: &str, volume: &str) -> String {
    format!("rekcod-snap.{project}.{name}.{volume}")
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

#[derive(Serialize)]
pub struct Snapshot {
    pub project: String,
    pub name: String,
    pub created: u64,
    /// Project volume -> snapshot volume.
    pub volumes: BTreeMap<String, String>,
}

fn snapshots(paths: &Paths, project: Option<&str>) -> Result<Vec<Snapshot>> {
    let filter = match project {
        Some(p) => format!("label={SNAP_PROJECT}={p}"),
        None => format!("label={SNAP}"),
    };
    let fmt = format!(
        "{{{{.Name}}}}\t{{{{.Label \"{SNAP_PROJECT}\"}}}}\t{{{{.Label \"{SNAP_NAME}\"}}}}\t{{{{.Label \"{SNAP_VOLUME}\"}}}}\t{{{{.Label \"{SNAP_CREATED}\"}}}}"
    );
    let mut by_key: BTreeMap<(String, String), Snapshot> = BTreeMap::new();
    for line in lines(&docker(paths, &["volume", "ls", "--filter", &filter, "--format", &fmt])?) {
        let f: Vec<&str> = line.split('\t').collect();
        let [vol, project, name, source, created] = f[..] else { continue };
        let s = by_key.entry((project.into(), name.into())).or_insert_with(|| Snapshot {
            project: project.into(),
            name: name.into(),
            created: 0,
            volumes: BTreeMap::new(),
        });
        s.created = s.created.max(created.parse().unwrap_or(0));
        s.volumes.insert(source.into(), vol.into());
    }
    Ok(by_key.into_values().collect())
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

pub fn save(paths: &Paths, explicit_project: Option<&str>, name: Option<&str>) -> Result<()> {
    let t0 = Instant::now();
    let project = project(paths, explicit_project)?;
    let name = snap_name(name, true)?;
    let volumes = project_volumes(paths, &project)?;
    if volumes.is_empty() {
        bail!("compose project `{project}` has no named volumes to snapshot (-p to pick another project)");
    }
    let created = now().to_string();
    let mut pairs = Vec::new();
    for v in &volumes {
        let snap = snapshot_volume(&project, &name, v);
        let labels = docker(paths, &["volume", "inspect", "--format", "{{json .Labels}}", v])?;
        let _ = docker(paths, &["volume", "rm", "-f", &snap]);
        docker(paths, &[
            "volume", "create",
            "--label", &format!("{SNAP}=1"),
            "--label", &format!("{SNAP_PROJECT}={project}"),
            "--label", &format!("{SNAP_NAME}={name}"),
            "--label", &format!("{SNAP_VOLUME}={v}"),
            "--label", &format!("{SNAP_CREATED}={created}"),
            "--label", &format!("{SNAP_LABELS}={labels}"),
            &snap,
        ])?;
        pairs.push((v.clone(), snap));
    }
    let running = running_containers(paths, &project)?;
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
    copied?;
    println!(
        "saved `{name}` for {project}: {} volume(s), {} container(s) paused, {:.1}s",
        pairs.len(),
        running.len(),
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

pub fn restore(paths: &Paths, explicit_project: Option<&str>, name: Option<&str>) -> Result<()> {
    let t0 = Instant::now();
    let project = project(paths, explicit_project)?;
    let name = snap_name(name, false)?;
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
    let pairs: Vec<(String, String)> = snap.volumes.iter().map(|(v, s)| (s.clone(), v.clone())).collect();
    let copied = copy_volumes(paths, &pairs);
    if !running.is_empty() {
        let started = docker(paths, &[&["start"][..], &running.iter().map(String::as_str).collect::<Vec<_>>()].concat());
        if let Err(e) = started {
            eprintln!("rekcod: warning: {e:#}");
        }
    }
    copied?;
    println!(
        "restored `{name}` for {project}: {} volume(s), {} container(s) restarted, {:.1}s",
        pairs.len(),
        running.len(),
        t0.elapsed().as_secs_f64()
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
    let snaps = snapshots(paths, Some(&project))?;
    let Some(snap) = snaps.iter().find(|s| s.name == name) else { bail!("no snapshot `{name}` for {project}") };
    let vols: Vec<&str> = snap.volumes.values().map(String::as_str).collect();
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
        assert_eq!(snapshot_volume("shop", "seeded", "shop_pg"), "rekcod-snap.shop.seeded.shop_pg");
    }
}
