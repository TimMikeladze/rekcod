//! `rekcod snap`: save and restore a compose project's named volumes
//! (docs/phase5.md §5, plan.md §3a).
//!
//! A snapshot of volume `v` is another volume, labelled so it can be found again. Save pauses
//! the project's running containers for a crash-consistent copy (databases recover through
//! their WAL, as after a power cut). Restore stops them, copies back, and starts the same
//! containers again. Copies run in one helper container with `cp -a`.
//!
//! Each save writes a new generation: volumes named `rekcod-snap.<project>.<name>.<gen>.<volume>`,
//! then, once every copy succeeded, a marker volume `rekcod-snap-done.<project>.<name>.<gen>`
//! labelled `dev.rekcod.snap.done` with a commit number one above the snapshot's last. The
//! complete generation with the highest commit number is the snapshot, whatever its `created`
//! time says (a pulled backup is older than the local copy it replaces). Save checks that its
//! generation is the one that counts now, and only then deletes the others; a save that fails
//! or is killed leaves the previous snapshot as it was. Everything that changes one snapshot
//! holds `run/snap/<project>.<name>.lock`, and whatever reads or changes the project's volumes
//! and containers also holds `run/snap/<project>.lock`, so a menu bar save and a terminal
//! restore take turns whatever their names. Restore first saves the current volumes as
//! `pre-restore`, and copies that back if the restore copy fails. Snapshots from before
//! generations (`rekcod-snap.<project>.<name>.<volume>`), and markers named `....<gen>.done`
//! from before markers had their own prefix, are still read.
//!
//! `save --push` uploads each snapshot volume as `snaps/<project>/<name>/<gen>/<volume>.tar.zst`,
//! then `snaps/<project>/<name>/manifest.json` with each archive's SHA-256 and size and the
//! generation it replaced, and then deletes generations more than an hour older than the one
//! it replaced, which no push still in flight can be. An interrupted push leaves the old
//! manifest pointing at its own archives. `restore --pull`
//! checks every archive against the manifest before using it, and still reads pushes from
//! before generations (`<volume>.tar.zst` beside the manifest, no hashes) (docs/rustfs.md).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::paths::Paths;
use crate::s3::{Body, S3};

const HELPER_IMAGE: &str = "alpine:3.24";
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
/// On the marker: the generation's commit number. Markers from before it count as 0.
const SNAP_COMMIT: &str = "dev.rekcod.snap.commit";
/// The name as given (a branch like `feature/x`), before it was made volume-safe.
const SNAP_RAW: &str = "dev.rekcod.snap.raw";
/// What `restore` saves the current volumes as before replacing them.
pub const PRE_RESTORE: &str = "pre-restore";
const PROJECT_LABEL: &str = "com.docker.compose.project";
const WORKDIR_LABEL: &str = "com.docker.compose.project.working_dir";

/// What snapshots need from Docker: the rekcod daemon through the docker CLI, a fake in tests.
trait Engine {
    /// `docker <args>`; stdout on success.
    fn docker(&self, args: &[&str]) -> Result<String>;
    /// `GET /volumes` for the volumes labelled `dev.rekcod.snap`.
    fn snap_volumes(&self) -> Result<Vec<u8>>;
    /// Writes a tar of `volume`'s contents to `w`.
    fn export(&self, volume: &str, w: &mut dyn Write) -> Result<()>;
    /// Unpacks the tar in `r` into `volume`.
    fn import(&self, volume: &str, r: &mut dyn Read) -> Result<()>;
    /// The rekcod home: locks and temporary archives go under it.
    fn home(&self) -> &Path;
    fn now(&self) -> u64 {
        now()
    }
}

struct Cli<'a>(&'a Paths);

impl Cli<'_> {
    fn command(&self) -> Result<std::process::Command> {
        let mut cmd = crate::docker::any_cli(self.0)?;
        cmd.args(["-H", &self.0.docker_host()]);
        Ok(cmd)
    }
}

impl Engine for Cli<'_> {
    fn docker(&self, args: &[&str]) -> Result<String> {
        let out = self.command()?.args(args).stdin(Stdio::null()).output()?;
        if !out.status.success() {
            bail!("docker {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn snap_volumes(&self) -> Result<Vec<u8>> {
        let filter = "%7B%22label%22%3A%5B%22dev.rekcod.snap%22%5D%7D"; // {"label":["dev.rekcod.snap"]}
        crate::cli::api_get(self.0, &format!("/volumes?filters={filter}"))
    }

    fn export(&self, volume: &str, w: &mut dyn Write) -> Result<()> {
        let mut child = self
            .command()?
            .args(["run", "--rm", "--network", "none", "-v", &format!("{volume}:/v:ro"), HELPER_IMAGE])
            .args(["tar", "-C", "/v", "-cf", "-", "."])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let copied = std::io::copy(child.stdout.as_mut().context("no stdout")?, w);
        if copied.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        copied.with_context(|| format!("tar of {volume}"))?;
        if !status.success() {
            bail!("tar of {volume} failed");
        }
        Ok(())
    }

    fn import(&self, volume: &str, r: &mut dyn Read) -> Result<()> {
        let mut child = self
            .command()?
            .args(["run", "--rm", "-i", "--network", "none", "-v", &format!("{volume}:/v"), HELPER_IMAGE])
            .args(["tar", "-C", "/v", "-xf", "-"])
            .stdin(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        let copied = std::io::copy(r, &mut stdin);
        drop(stdin);
        let failed_to_read = copied.as_ref().is_err_and(|e| e.kind() != std::io::ErrorKind::BrokenPipe);
        if failed_to_read {
            let _ = child.kill();
        }
        let status = child.wait()?;
        if failed_to_read || status.success() {
            copied.with_context(|| format!("untar into {volume}"))?;
        }
        if !status.success() {
            bail!("untar into {volume} failed");
        }
        Ok(())
    }

    fn home(&self) -> &Path {
        &self.0.home
    }
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

/// Its own prefix: a source volume named `done` used to give its data volume the marker's name.
fn marker_volume(project: &str, name: &str, gen_: &str) -> String {
    format!("rekcod-snap-done.{project}.{name}.{gen_}")
}

/// `-p`, then `COMPOSE_PROJECT_NAME`, then the project whose containers were
/// started from this directory, then the directory name.
fn project(d: &dyn Engine, explicit: Option<&str>) -> Result<String> {
    if let Some(p) = explicit.map(str::to_string).or_else(|| std::env::var("COMPOSE_PROJECT_NAME").ok()).filter(|p| !p.is_empty()) {
        return Ok(normalize_project(&p));
    }
    let cwd = std::env::current_dir()?;
    let filter = format!("label={WORKDIR_LABEL}={}", cwd.display());
    let fmt = format!("{{{{.Label \"{PROJECT_LABEL}\"}}}}");
    if let Some(p) = lines(&d.docker(&["ps", "-a", "--filter", &filter, "--format", &fmt])?).into_iter().next() {
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

fn project_volumes(d: &dyn Engine, project: &str) -> Result<Vec<String>> {
    let filter = format!("label={PROJECT_LABEL}={project}");
    Ok(lines(&d.docker(&["volume", "ls", "--filter", &filter, "--format", "{{.Name}}"])?))
}

/// The project's containers and their states (`running`, `paused`, `restarting`, `exited`, ...).
fn containers(d: &dyn Engine, project: &str) -> Result<Vec<(String, String)>> {
    let filter = format!("label={PROJECT_LABEL}={project}");
    let out = d.docker(&["ps", "-a", "--filter", &filter, "--format", "{{.ID}} {{.State}}"])?;
    Ok(lines(&out).into_iter().filter_map(|l| l.split_once(' ').map(|(id, st)| (id.to_string(), st.to_string()))).collect())
}

/// What save pauses: the running ones. One that is restarting can't be paused and would
/// write during the copy, so the save waits for nobody and refuses.
fn to_pause(d: &dyn Engine, project: &str) -> Result<Vec<String>> {
    let all = containers(d, project)?;
    if let Some((id, _)) = all.iter().find(|(_, st)| st == "restarting") {
        bail!("container {id} of {project} is restarting; stop it or let it settle, then save");
    }
    Ok(all.into_iter().filter(|(_, st)| st == "running").map(|(id, _)| id).collect())
}

/// What restore stops and starts again: running and restarting ones. A paused one would
/// carry on with its old files open over the restored ones, so restore refuses.
fn to_stop(d: &dyn Engine, project: &str) -> Result<Vec<String>> {
    let all = containers(d, project)?;
    if let Some((id, _)) = all.iter().find(|(_, st)| st == "paused") {
        bail!("container {id} of {project} is paused; `docker unpause {id}` first");
    }
    Ok(all.into_iter().filter(|(_, st)| st == "running" || st == "restarting").map(|(id, _)| id).collect())
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
    /// The live generation; empty for a snapshot from before generations.
    #[serde(skip)]
    pub generation: String,
    #[serde(skip)]
    pub commit: u64,
}

#[derive(Default)]
struct Generation {
    created: u64,
    done: bool,
    commit: u64,
    raw: Option<String>,
    volumes: BTreeMap<String, String>,
    markers: Vec<String>,
}

/// Snapshots from `GET /volumes`: for each (project, name) the complete generation with the
/// highest commit number (then the newest).
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
            g.commit = g.commit.max(l[SNAP_COMMIT].as_str().and_then(|c| c.parse().ok()).unwrap_or(0));
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
        let live =
            gens.iter().filter(|(_, g)| g.done && !g.volumes.is_empty()).max_by_key(|(_, g)| (g.commit, g.created)).map(|(k, _)| k.clone());
        let mut stale = Vec::new();
        for (k, g) in &gens {
            if Some(k) != live.as_ref() {
                stale.extend(g.volumes.values().cloned());
                stale.extend(g.markers.iter().cloned());
            }
        }
        if let Some((generation, g)) = live.and_then(|k| gens.into_iter().find(|(gk, _)| *gk == k)) {
            out.push(Snapshot { project, name, created: g.created, volumes: g.volumes, stale, raw: g.raw, generation, commit: g.commit });
        } else if !stale.is_empty() {
            // Only unfinished saves: nothing to restore, but `rm` can clean them up.
            out.push(Snapshot {
                project,
                name,
                created: 0,
                volumes: BTreeMap::new(),
                stale,
                raw: None,
                generation: String::new(),
                commit: 0,
            });
        }
    }
    out
}

/// Snapshots on the VM, restorable ones only, optionally for one project.
fn snapshots(d: &dyn Engine, project: Option<&str>) -> Result<Vec<Snapshot>> {
    Ok(all_snapshots(d)?.into_iter().filter(|s| !s.volumes.is_empty() && project.is_none_or(|p| s.project == p)).collect())
}

fn all_snapshots(d: &dyn Engine) -> Result<Vec<Snapshot>> {
    Ok(parse(&d.snap_volumes()?))
}

fn find(d: &dyn Engine, project: &str, name: &str) -> Result<Option<Snapshot>> {
    Ok(all_snapshots(d)?.into_iter().find(|s| s.project == project && s.name == name))
}

/// Held while snapshot `name` of `project` changes, so two saves, or a save and a restore
/// (menu bar and terminal), take turns instead of deleting each other's generation.
fn lock(d: &dyn Engine, project: &str, name: &str) -> Result<std::fs::File> {
    let dir = d.home().join("run/snap");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{project}.{name}.lock"));
    if crate::paths::locked(&path) {
        eprintln!("rekcod: waiting for another change to snapshot `{name}` of {project}");
    }
    crate::paths::lock(&path)
}

/// Held while the project's volumes or containers are read or changed: saves and restores
/// of different snapshots of one project take turns too.
fn lock_project(d: &dyn Engine, project: &str) -> Result<std::fs::File> {
    let dir = d.home().join("run/snap");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{project}.lock"));
    if crate::paths::locked(&path) {
        eprintln!("rekcod: waiting for another snapshot change to {project}");
    }
    crate::paths::lock(&path)
}

/// Where push and pull keep an archive on its way: `run/snap/tmp/snap-<what>-<pid>.tar.zst`.
/// Files a killed process left there (its pid is gone) are deleted first.
fn tmp_archive(d: &dyn Engine, what: &str) -> Result<std::path::PathBuf> {
    let dir = d.home().join("run/snap/tmp");
    std::fs::create_dir_all(&dir)?;
    for e in std::fs::read_dir(&dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name.strip_suffix(".tar.zst").and_then(|n| n.rsplit('-').next()).and_then(|p| p.parse::<libc::pid_t>().ok());
        if let Some(pid) = pid
            && unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(dir.join(format!("snap-{what}-{}.tar.zst", std::process::id())))
}

fn ensure_helper(d: &dyn Engine) -> Result<()> {
    if d.docker(&["image", "inspect", HELPER_IMAGE]).is_err() {
        d.docker(&["pull", "-q", HELPER_IMAGE])?;
    }
    Ok(())
}

/// Fails unless the VM's volume disk has room for `volumes` once more, with 10% to spare:
/// sizes from `du`, free space from `df`, in one helper container.
fn check_room(d: &dyn Engine, volumes: &[&str], what: &str) -> Result<()> {
    if volumes.is_empty() {
        return Ok(());
    }
    ensure_helper(d)?;
    let mut args: Vec<String> = vec!["run".into(), "--rm".into(), "--network".into(), "none".into()];
    for (i, v) in volumes.iter().enumerate() {
        args.extend(["-v".into(), format!("{v}:/v/{i}:ro")]);
    }
    let script = "echo $(du -sck /v/* | tail -1 | cut -f1) $(df -Pk /v/0 | tail -1 | awk '{print $4}')";
    args.extend([HELPER_IMAGE.into(), "sh".into(), "-c".into(), script.into()]);
    let out = d.docker(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    let mut n = out.split_whitespace().filter_map(|x| x.parse::<u64>().ok());
    let (Some(need), Some(free)) = (n.next(), n.next()) else { bail!("can't read the VM's free space from {out:?}") };
    if need + need / 10 > free {
        bail!(
            "{what} needs about {} MiB on the VM's disk and {} MiB are free; free some (docker system prune, rekcod snap rm) or grow the disk",
            need / 1024 + 1,
            free / 1024
        );
    }
    Ok(())
}

/// Replaces each `to` volume's contents with its `from` volume's, in one helper container.
fn copy_volumes(d: &dyn Engine, pairs: &[(String, String)]) -> Result<()> {
    ensure_helper(d)?;
    let mut args: Vec<String> = vec!["run".into(), "--rm".into(), "--network".into(), "none".into()];
    let mut script = String::from("set -e");
    for (i, (from, to)) in pairs.iter().enumerate() {
        args.extend(["-v".into(), format!("{from}:/from/{i}:ro"), "-v".into(), format!("{to}:/to/{i}")]);
        script.push_str(&format!("; find /to/{i} -mindepth 1 -delete; cp -a /from/{i}/. /to/{i}/"));
    }
    args.extend([HELPER_IMAGE.into(), "sh".into(), "-c".into(), script]);
    d.docker(&args.iter().map(String::as_str).collect::<Vec<_>>()).map(|_| ())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn snap_prefix(project: &str, name: &str) -> String {
    format!("snaps/{project}/{name}")
}

fn manifest_key(prefix: &str) -> String {
    format!("{prefix}/manifest.json")
}

fn archive_key(prefix: &str, gen_: &str, volume: &str) -> String {
    format!("{prefix}/{gen_}/{volume}.tar.zst")
}

/// Docker's volume name rule, so a manifest can't name a key outside its snapshot.
fn valid_volume(v: &str) -> bool {
    v.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// Counts and hashes what passes through to `w`.
struct Hashed<W> {
    w: W,
    sha: Sha256,
    n: u64,
}

impl<W: Write> Hashed<W> {
    fn new(w: W) -> Hashed<W> {
        Hashed { w, sha: Sha256::new(), n: 0 }
    }

    /// Bytes written and their SHA-256 (hex).
    fn finish(self) -> (u64, String) {
        (self.n, crate::s3::hex(&self.sha.finalize()))
    }
}

impl<W: Write> Write for Hashed<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.w.write(buf)?;
        self.sha.update(&buf[..n]);
        self.n += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

/// A committed generation, as `push` uploads it.
struct Saved {
    gen_: String,
    created: u64,
    /// (project volume, snapshot volume).
    pairs: Vec<(String, String)>,
    /// The name as given (a branch like `feature/x`).
    raw: Option<String>,
}

/// Uploads generation `saved` of snapshot `name`: each volume under the generation's own
/// prefix, then the manifest that points at them. Until the manifest is written, `pull` gets
/// the previous push. Old generations are deleted after (see [`prune`]).
fn push(d: &dyn Engine, s3: &S3, bucket: &str, project: &str, name: &str, saved: &Saved) -> Result<()> {
    let staged = upload(d, s3, bucket, project, name, saved)?;
    commit(s3, bucket, project, name, saved, staged)
}

/// What [`upload`] put in the bucket, for the manifest.
struct Staged {
    labels: serde_json::Map<String, serde_json::Value>,
    archives: serde_json::Map<String, serde_json::Value>,
}

/// The archives of `saved`, each checked to be there. On failure the ones it put are deleted.
fn upload(d: &dyn Engine, s3: &S3, bucket: &str, project: &str, name: &str, saved: &Saved) -> Result<Staged> {
    let prefix = snap_prefix(project, name);
    let mut labels = serde_json::Map::new();
    let mut archives = serde_json::Map::new();
    let mut uploaded = Vec::new();
    let tmp = tmp_archive(d, "push")?;
    let staged = (|| -> Result<()> {
        for (v, snap) in &saved.pairs {
            let key = archive_key(&prefix, &saved.gen_, v);
            uploaded.push(key.clone());
            let r = (|| -> Result<(u64, String)> {
                let mut enc = zstd::Encoder::new(Hashed::new(std::fs::File::create(&tmp)?), 3)?;
                d.export(snap, &mut enc)?;
                let (size, sha256) = enc.finish()?.finish();
                s3.put(bucket, &key, Body::File(&tmp))?;
                Ok((size, sha256))
            })();
            let _ = std::fs::remove_file(&tmp);
            let (size, sha256) = r?;
            let l = d.docker(&["volume", "inspect", "--format", "{{json .Labels}}", v])?;
            labels.insert(v.clone(), serde_json::from_str(&l).unwrap_or(serde_json::Value::Null));
            archives.insert(v.clone(), serde_json::json!({"sha256": sha256, "size": size}));
        }
        // Nothing points at these yet: check they all arrived before the manifest does.
        for key in &uploaded {
            match s3.request("HEAD", &format!("/{bucket}/{key}"), &[], Body::Empty)?.status {
                200 => {}
                status => bail!("{bucket}/{key} isn't there after uploading it (HTTP {status})"),
            }
        }
        Ok(())
    })();
    if let Err(e) = staged {
        for key in &uploaded {
            let _ = s3.delete(bucket, key);
        }
        return Err(e.context(format!("push `{name}` failed; what `--pull` gets is unchanged")));
    }
    Ok(Staged { labels, archives })
}

/// Writes the manifest for `saved`, then prunes. A push of the same snapshot from another Mac
/// may land between the two; it keeps its archives, and whichever manifest is written last
/// is what `pull` gets.
fn commit(s3: &S3, bucket: &str, project: &str, name: &str, saved: &Saved, staged: Staged) -> Result<()> {
    let prefix = snap_prefix(project, name);
    let previous = s3
        .get_opt(bucket, &manifest_key(&prefix))
        .map(|m| m.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()?["generation"].as_str().map(String::from)));
    let mut m = serde_json::json!({
        "version": 2,
        "project": project,
        "name": name,
        "created": saved.created,
        "generation": saved.gen_,
        // The field pushes had before generations: an older rekcod reads it, asks for the
        // old-layout archives that `prune` deletes, and fails instead of finding no volumes.
        "volumes": staged.labels,
        "archives": staged.archives,
    });
    if let Ok(Some(p)) = &previous {
        m["previous"] = serde_json::json!(p);
    }
    if let Some(raw) = saved.raw.as_deref().filter(|r| *r != name) {
        m["raw"] = serde_json::json!(raw);
    }
    s3.put(bucket, &manifest_key(&prefix), Body::Bytes(m.to_string().as_bytes()))?;
    let now = s3
        .get_opt(bucket, &manifest_key(&prefix))
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()?["generation"].as_str().map(String::from));
    if now.as_deref().is_some_and(|g| g != saved.gen_) {
        eprintln!("rekcod: warning: another push of `{name}` replaced this one just now; `--pull` gets that one");
    }
    let pruned = previous.and_then(|p| prune(s3, bucket, &prefix, &saved.gen_, p.as_deref()));
    if let Err(e) = pruned {
        eprintln!("rekcod: warning: kept older archives under {bucket}/{prefix}: {e:#}");
    }
    println!("pushed `{name}` to {}/{bucket}/{prefix}", s3.endpoint);
    Ok(())
}

/// When generation `g` was saved: its id starts with the Unix time.
fn gen_created(g: &str) -> Option<u64> {
    g.get(..g.len().checked_sub(4)?)?.parse().ok()
}

/// Deletes under `prefix` the archives of the old layout (`<prefix>/<volume>.tar.zst`), so a
/// rekcod from before generations fails to pull a newer push instead of restoring an older
/// one, and generations saved more than an hour before the older of `new` and `previous`.
/// Newer ones may belong to a push from another Mac that hasn't written its manifest yet.
fn prune(s3: &S3, bucket: &str, prefix: &str, new: &str, previous: Option<&str>) -> Result<()> {
    let under = format!("{prefix}/");
    let oldest = [Some(new), previous].into_iter().flatten().filter_map(gen_created).min().unwrap_or(0);
    let mut doomed = Vec::new();
    for o in s3.list(bucket, &under)? {
        let Some(rest) = o.key.strip_prefix(&under) else { continue };
        let old = match rest.split_once('/') {
            None => rest != "manifest.json",
            Some((g, _)) => Some(g) != Some(new) && Some(g) != previous && gen_created(g).is_some_and(|t| t + 3600 < oldest),
        };
        if old {
            doomed.push((o.key, None));
        }
    }
    if doomed.is_empty() { Ok(()) } else { s3.delete_many(bucket, &doomed) }
}

/// One volume of a pushed snapshot.
struct Archive {
    volume: String,
    labels: String,
    key: String,
    /// SHA-256 (hex) and size; pushes from before generations have neither.
    check: Option<(String, u64)>,
}

fn archives(m: &serde_json::Value, prefix: &str) -> Result<Vec<Archive>> {
    let generation = m["generation"].as_str();
    if let Some(g) = generation
        && (g.is_empty() || !g.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        bail!("{}: bad generation {g:?}", manifest_key(prefix));
    }
    let mut out = Vec::new();
    for (v, labels) in m["volumes"].as_object().into_iter().flatten() {
        if !valid_volume(v) {
            bail!("{}: bad volume name {v:?}", manifest_key(prefix));
        }
        let (key, check) = match generation {
            None => (format!("{prefix}/{v}.tar.zst"), None),
            Some(g) => {
                let a = &m["archives"][v];
                let (Some(sha256), Some(size)) = (a["sha256"].as_str(), a["size"].as_u64()) else {
                    bail!("{}: no checksum for {v}", manifest_key(prefix));
                };
                (archive_key(prefix, g, v), Some((sha256.to_string(), size)))
            }
        };
        out.push(Archive { volume: v.clone(), labels: labels.to_string(), key, check });
    }
    if out.is_empty() {
        bail!("{} lists no volumes", manifest_key(prefix));
    }
    Ok(out)
}

/// Recreates snapshot `name` of `project` from what `push` uploaded, replacing a local one.
fn pull(d: &dyn Engine, s3: &S3, bucket: &str, project: &str, name: &str) -> Result<()> {
    let prefix = snap_prefix(project, name);
    let b = s3.get_opt(bucket, &manifest_key(&prefix))?.ok_or_else(|| anyhow!("no pushed snapshot {prefix}"))?;
    let m: serde_json::Value = serde_json::from_slice(&b).with_context(|| manifest_key(&prefix))?;
    let remote = archives(&m, &prefix)?;
    let created = m["created"].as_u64().unwrap_or_else(|| d.now());
    // The branch name the push was saved under, so a save of that branch here matches it.
    let raw = m["raw"].as_str();
    ensure_helper(d)?;
    let gen_ = new_gen(created)?;
    let mut made = BTreeMap::new();
    let tmp = tmp_archive(d, "pull")?;
    let r = (|| -> Result<()> {
        for a in &remote {
            let snap = create_snap_volume(d, project, name, raw, &gen_, created, &a.volume, &a.labels)?;
            made.insert(a.volume.clone(), snap.clone());
            let mut f = Hashed::new(std::fs::File::create(&tmp)?);
            s3.get(bucket, &a.key, &mut f)?;
            let (size, sha256) = f.finish();
            if let Some((want_sha256, want_size)) = &a.check
                && (sha256 != *want_sha256 || size != *want_size)
            {
                bail!("{bucket}/{}: {size} bytes with SHA-256 {sha256}, but the manifest says {want_size} bytes, {want_sha256}", a.key);
            }
            d.import(&snap, &mut zstd::Decoder::new(std::fs::File::open(&tmp)?)?)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = r {
        remove_volumes(d, &made.into_values().collect::<Vec<_>>());
        return Err(e);
    }
    finish(d, project, name, &gen_, created, &made)?;
    eprintln!("rekcod: pulled `{name}` for {project} from {}/{bucket}/{prefix}", s3.endpoint);
    Ok(())
}

/// A generation id: the save's time plus a few random letters.
fn new_gen(created: u64) -> Result<String> {
    Ok(format!("{created}{}", &crate::sandbox::new_id()?[..4]))
}

#[allow(clippy::too_many_arguments)]
fn create_snap_volume(
    d: &dyn Engine,
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
    d.docker(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(snap)
}

fn remove_volumes(d: &dyn Engine, vols: &[String]) {
    if !vols.is_empty() {
        let _ = d.docker(&[&["volume", "rm", "-f"][..], &vols.iter().map(String::as_str).collect::<Vec<_>>()].concat());
    }
}

/// Commits generation `gen_`, whose volumes are `volumes` (project volume -> snapshot volume):
/// writes its marker with the next commit number, checks that it is now the snapshot, then
/// deletes every other generation. If the check fails, the new generation is deleted instead.
fn finish(d: &dyn Engine, project: &str, name: &str, gen_: &str, created: u64, volumes: &BTreeMap<String, String>) -> Result<()> {
    let marker = marker_volume(project, name, gen_);
    let live = find(d, project, name)
        .and_then(|before| {
            let commit = before.map_or(0, |s| s.commit) + 1;
            d.docker(&[
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
                "--label",
                &format!("{SNAP_COMMIT}={commit}"),
                &marker,
            ])
        })
        .and_then(|_| find(d, project, name));
    match live {
        Ok(Some(s)) if s.generation == gen_ && s.volumes == *volumes => {
            remove_volumes(d, &s.stale);
            Ok(())
        }
        other => {
            let mut ours: Vec<String> = volumes.values().cloned().collect();
            ours.push(marker.clone());
            remove_volumes(d, &ours);
            let why = other.err().map(|e| format!(": {e:#}")).unwrap_or_else(|| format!(": {marker} didn't mark it complete"));
            bail!("snapshot `{name}` for {project} wasn't saved{why}; the previous one is unchanged")
        }
    }
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

/// Unpauses, when dropped, every container it paused, so neither a pause that fails halfway
/// through the project nor a failed copy leaves containers frozen.
struct Paused<'a> {
    d: &'a dyn Engine,
    ids: Vec<String>,
}

impl Paused<'_> {
    fn pause(&mut self, id: &str) -> Result<()> {
        let r = self.d.docker(&["pause", id]);
        // A pause that errors after dockerd applied it (a dropped connection) still counts.
        if r.is_ok() || self.d.docker(&["inspect", "--format", "{{.State.Paused}}", id]).is_ok_and(|p| p == "true") {
            self.ids.push(id.to_string());
        }
        r.map(|_| ())
    }
}

impl Drop for Paused<'_> {
    fn drop(&mut self) {
        if !self.ids.is_empty()
            && let Err(e) = self.d.docker(&[&["unpause"][..], &self.ids.iter().map(String::as_str).collect::<Vec<_>>()].concat())
        {
            eprintln!("rekcod: warning: {e:#}");
        }
    }
}

pub fn save(paths: &Paths, explicit_project: Option<&str>, name: Option<&str>, push_it: bool) -> Result<()> {
    save_with(&Cli(paths), explicit_project, name, push_it.then_some(|| crate::storage::snap_target(paths)))
}

fn save_with(
    d: &dyn Engine,
    explicit_project: Option<&str>,
    name: Option<&str>,
    push_to: Option<impl FnOnce() -> Result<(S3, String)>>,
) -> Result<()> {
    let project = project(d, explicit_project)?;
    let raw = match name {
        Some(n) => n.to_string(),
        None => git_branch().unwrap_or_default(),
    };
    let name = snap_name(name, true)?;
    // Where it goes is checked before anything is paused.
    let target = push_to.map(|t| t()).transpose()?;
    let _lock = lock(d, &project, &name)?;
    if let Some(other) = find(d, &project, &name)?
        && other.raw.as_deref().unwrap_or(&other.name) != raw.as_str()
        && !raw.is_empty()
    {
        bail!(
            "`{raw}` and `{}` both become snapshot `{name}`; pass a name: rekcod snap save <name>",
            other.raw.as_deref().unwrap_or(&other.name)
        );
    }
    let saved = {
        let _project = lock_project(d, &project)?;
        save_as(d, &project, &name, Some(&raw))?
    };
    if let Some((s3, bucket)) = target {
        push(d, &s3, &bucket, &project, &name, &saved)?;
    }
    Ok(())
}

/// Saves the project's volumes as a new generation of `name` and commits it. The caller
/// holds `name`'s lock and the project's.
fn save_as(d: &dyn Engine, project: &str, name: &str, raw: Option<&str>) -> Result<Saved> {
    let t0 = Instant::now();
    let volumes = project_volumes(d, project)?;
    if volumes.is_empty() {
        bail!("compose project `{project}` has no named volumes to snapshot (-p to pick another project)");
    }
    let created = d.now();
    let gen_ = new_gen(created)?;
    let mut pairs = Vec::new();
    let copied = (|| -> Result<usize> {
        for v in &volumes {
            let labels = d.docker(&["volume", "inspect", "--format", "{{json .Labels}}", v])?;
            pairs.push((v.clone(), create_snap_volume(d, project, name, raw, &gen_, created, v, &labels)?));
        }
        let running = to_pause(d, project)?;
        let _held = HoldSignals::new();
        let mut paused = Paused { d, ids: Vec::new() };
        for id in &running {
            paused.pause(id)?;
        }
        copy_volumes(d, &pairs)?;
        Ok(running.len())
    })();
    let paused = match copied {
        Ok(n) => n,
        Err(e) => {
            // The previous snapshot of this name is untouched.
            remove_volumes(d, &pairs.into_iter().map(|(_, s)| s).collect::<Vec<_>>());
            return Err(e);
        }
    };
    finish(d, project, name, &gen_, created, &pairs.iter().cloned().collect())?;
    println!("saved `{name}` for {project}: {} volume(s), {paused} container(s) paused, {:.1}s", pairs.len(), t0.elapsed().as_secs_f64());
    Ok(Saved { gen_, created, pairs, raw: raw.map(String::from) })
}

pub fn restore(paths: &Paths, explicit_project: Option<&str>, name: Option<&str>, pull_it: bool) -> Result<()> {
    restore_with(&Cli(paths), explicit_project, name, pull_it.then_some(|| crate::storage::snap_target(paths)))
}

fn restore_with(
    d: &dyn Engine,
    explicit_project: Option<&str>,
    name: Option<&str>,
    pull_from: Option<impl FnOnce() -> Result<(S3, String)>>,
) -> Result<()> {
    let t0 = Instant::now();
    let project = project(d, explicit_project)?;
    let name = snap_name(name, false)?;
    let _lock = lock(d, &project, &name)?;
    if let Some(source) = pull_from {
        let (s3, bucket) = source()?;
        pull(d, &s3, &bucket, &project, &name)?;
    }
    // Before the project's: a save of `pre-restore` takes them in that order too.
    let _pre = if name != PRE_RESTORE { Some(lock(d, &project, PRE_RESTORE)?) } else { None };
    let _project = lock_project(d, &project)?;
    let all = snapshots(d, Some(&project))?;
    let Some(snap) = all.iter().find(|s| s.name == name) else {
        let have: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        bail!("no snapshot `{name}` for {project} (have: {})", if have.is_empty() { "none".into() } else { have.join(", ") });
    };
    for (v, s) in &snap.volumes {
        if d.docker(&["volume", "inspect", v]).is_err() {
            // Gone after `down -v`: recreate it with the labels compose expects.
            let fmt = format!("{{{{index .Labels \"{SNAP_LABELS}\"}}}}");
            let labels: BTreeMap<String, String> =
                serde_json::from_str(&d.docker(&["volume", "inspect", "--format", &fmt, s])?).ok().flatten().unwrap_or_default();
            let mut args: Vec<String> = vec!["volume".into(), "create".into()];
            for (k, val) in labels {
                args.extend(["--label".into(), format!("{k}={val}")]);
            }
            args.push(v.clone());
            d.docker(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
    }
    // `pre-restore` copies the live volumes, then the snapshot is copied over them: room
    // for both, checked before anything stops.
    let mut sizes: Vec<&str> = snap.volumes.values().map(String::as_str).collect();
    if name != PRE_RESTORE {
        sizes.extend(snap.volumes.keys().map(String::as_str));
    }
    check_room(d, &sizes, &format!("restoring `{name}`"))?;
    let running = to_stop(d, &project)?;
    let start = |ids: &[String]| {
        if !ids.is_empty()
            && let Err(e) = d.docker(&[&["start"][..], &ids.iter().map(String::as_str).collect::<Vec<_>>()].concat())
        {
            eprintln!("rekcod: warning: {e:#}");
        }
    };
    if !running.is_empty() {
        let ids: Vec<&str> = running.iter().map(String::as_str).collect();
        if let Err(e) = d.docker(&[&["stop"][..], &ids].concat()) {
            // `docker stop a b` can stop `a` and then fail on `b`.
            start(&running);
            return Err(e);
        }
    }
    // What's there now, so a restore to the wrong snapshot, or one that fails half way, can
    // be undone.
    if name != PRE_RESTORE
        && let Err(e) = save_as(d, &project, PRE_RESTORE, None)
    {
        // Nothing was copied yet: the volumes are as they were.
        start(&running);
        return Err(e.context(format!("saving `{PRE_RESTORE}` first failed; nothing was restored")));
    }
    let pairs: Vec<(String, String)> = snap.volumes.iter().map(|(v, s)| (s.clone(), v.clone())).collect();
    if let Err(e) = copy_volumes(d, &pairs) {
        // Some volumes may hold the snapshot and some the old data: put back what was there.
        let back = (name != PRE_RESTORE)
            .then(|| find(d, &project, PRE_RESTORE))
            .transpose()?
            .flatten()
            .map(|p| p.volumes.iter().map(|(v, s)| (s.clone(), v.clone())).collect::<Vec<_>>());
        return match back.map(|b| copy_volumes(d, &b)) {
            Some(Ok(())) => {
                start(&running);
                Err(e.context(format!("restoring `{name}` failed; the volumes are back as they were (snapshot `{PRE_RESTORE}`)")))
            }
            Some(Err(e2)) => Err(e.context(format!(
                "restoring `{name}` failed, and so did copying `{PRE_RESTORE}` back ({e2:#}); the containers stay stopped. Free up space, then: rekcod snap restore {PRE_RESTORE} -p {project}"
            ))),
            None => Err(e.context(format!(
                "restoring `{name}` failed part way; the containers stay stopped. Free up space, then run it again: rekcod snap restore {name} -p {project}"
            ))),
        };
    }
    start(&running);
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
    let d = Cli(paths);
    let project = if all { None } else { Some(project(&d, explicit_project)?) };
    let snaps = snapshots(&d, project.as_deref())?;
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
    let d = Cli(paths);
    let project = project(&d, explicit_project)?;
    let name = normalize_name(name);
    let _lock = lock(&d, &project, &name)?;
    let Some(snap) = find(&d, &project, &name)? else { bail!("no snapshot `{name}` for {project}") };
    let mut vols: Vec<&str> = snap.volumes.values().map(String::as_str).collect();
    vols.extend(snap.stale.iter().map(String::as_str));
    d.docker(&[&["volume", "rm"][..], &vols].concat())?;
    println!("removed `{name}` for {project}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Vol {
        labels: BTreeMap<String, String>,
        data: Vec<u8>,
    }

    #[derive(Default)]
    struct World {
        vols: BTreeMap<String, Vol>,
        /// Container id -> (project, "running" | "paused" | "exited").
        containers: BTreeMap<String, (String, &'static str)>,
        /// Commands (args joined by spaces) that fail the next time they run.
        fail: Vec<String>,
        /// Markers get no labels, as when one reused an existing volume's name.
        unlabelled_markers: bool,
        /// The next `stop` stops its first container and then fails.
        stop_fails_after_first: bool,
        /// The next `run` whose command line contains this copies its first pair, then fails
        /// (a disk that fills up).
        run_fails_part_way: Option<String>,
        /// Every command, as run.
        log: Vec<String>,
        /// KiB the VM's disk has free, for the room check (plenty when unset).
        free_kib: Option<u64>,
    }

    /// Docker as far as snapshots use it, in memory. "Tar" is the volume's bytes as they are.
    struct Fake {
        home: PathBuf,
        now: std::sync::atomic::AtomicU64,
        w: Mutex<World>,
    }

    impl Fake {
        fn new(now: u64) -> Fake {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let home = std::env::temp_dir().join(format!("rekcod-snap-test-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).unwrap();
            Fake { home, now: now.into(), w: Mutex::new(World::default()) }
        }

        fn volume(&self, name: &str, project: &str, data: &[u8]) {
            let labels = BTreeMap::from([(PROJECT_LABEL.to_string(), project.to_string())]);
            self.w.lock().unwrap().vols.insert(name.into(), Vol { labels, data: data.to_vec() });
        }

        fn write(&self, name: &str, data: &[u8]) {
            self.w.lock().unwrap().vols.get_mut(name).unwrap().data = data.to_vec();
        }

        fn data(&self, name: &str) -> Vec<u8> {
            self.w.lock().unwrap().vols[name].data.clone()
        }

        fn names(&self) -> Vec<String> {
            self.w.lock().unwrap().vols.keys().cloned().collect()
        }

        fn container(&self, id: &str, project: &str) {
            self.w.lock().unwrap().containers.insert(id.into(), (project.into(), "running"));
        }

        fn state(&self, id: &str) -> &'static str {
            self.w.lock().unwrap().containers[id].1
        }

        fn fail(&self, command: &str) {
            self.w.lock().unwrap().fail.push(command.into());
        }

        /// The live snapshot's data, by project volume.
        fn snapshot(&self, project: &str, name: &str) -> BTreeMap<String, Vec<u8>> {
            let s = find(self, project, name).unwrap().expect("a snapshot");
            s.volumes.iter().map(|(v, snap)| (v.clone(), self.data(snap))).collect()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn label_filter(args: &[&str]) -> Option<(String, String)> {
        let i = args.iter().position(|a| *a == "--filter")?;
        let (k, v) = args[i + 1].strip_prefix("label=")?.split_once('=')?;
        Some((k.into(), v.into()))
    }

    impl Engine for Fake {
        fn docker(&self, args: &[&str]) -> Result<String> {
            let mut w = self.w.lock().unwrap();
            let line = args.join(" ");
            w.log.push(line.clone());
            if let Some(i) = w.fail.iter().position(|f| line.starts_with(f.as_str())) {
                w.fail.remove(i);
                bail!("docker {line}: failed (test)");
            }
            let set = |w: &mut World, ids: &[&str], state: &'static str| -> Result<String> {
                for id in ids {
                    w.containers.get_mut(*id).context("no such container")?.1 = state;
                }
                Ok(String::new())
            };
            match args {
                ["volume", "ls", ..] => {
                    let (k, v) = label_filter(args).unwrap();
                    Ok(w.vols.iter().filter(|(_, x)| x.labels.get(&k) == Some(&v)).map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("\n"))
                }
                ["volume", "inspect", "--format", f, n] => {
                    let x = w.vols.get(*n).context("no such volume")?;
                    match f.split('"').nth(1) {
                        Some(k) => Ok(x.labels.get(k).cloned().unwrap_or_default()),
                        None => Ok(serde_json::to_string(&x.labels)?),
                    }
                }
                ["volume", "inspect", n] => w.vols.get(*n).map(|_| String::new()).context("no such volume"),
                ["volume", "create", rest @ ..] => {
                    let (name, opts) = rest.split_last().unwrap();
                    let mut labels = BTreeMap::new();
                    for o in opts.chunks(2) {
                        let (k, v) = o[1].split_once('=').unwrap();
                        labels.insert(k.to_string(), v.to_string());
                    }
                    if w.unlabelled_markers && name.starts_with("rekcod-snap-done.") {
                        labels.clear();
                    }
                    // Like Docker: creating a volume that exists keeps its labels and data.
                    w.vols.entry(name.to_string()).or_insert(Vol { labels, data: Vec::new() });
                    Ok(name.to_string())
                }
                ["volume", "rm", rest @ ..] => {
                    for n in rest.iter().filter(|n| **n != "-f") {
                        w.vols.remove(*n);
                    }
                    Ok(String::new())
                }
                ["ps", "-a", ..] => {
                    let (key, project) = label_filter(args).unwrap();
                    if key != PROJECT_LABEL {
                        return Ok(String::new());
                    }
                    let mine = w.containers.iter().filter(|(_, (p, _))| *p == project);
                    Ok(mine.map(|(id, (_, st))| format!("{id} {st}")).collect::<Vec<_>>().join("\n"))
                }
                ["pause", id] => {
                    if w.containers.get(*id).context("no such container")?.1 != "running" {
                        bail!("container {id} is not running");
                    }
                    set(&mut w, &[*id], "paused")
                }
                ["inspect", "--format", "{{.State.Paused}}", id] => {
                    Ok((w.containers.get(*id).context("no such container")?.1 == "paused").to_string())
                }
                ["unpause", ids @ ..] | ["start", ids @ ..] => set(&mut w, ids, "running"),
                ["stop", ids @ ..] if std::mem::take(&mut w.stop_fails_after_first) => {
                    set(&mut w, &ids[..1], "exited")?;
                    bail!("docker {line}: failed after the first (test)")
                }
                ["stop", ids @ ..] => set(&mut w, ids, "exited"),
                ["image", "inspect", _] => Ok(String::new()),
                ["run", .., "sh", "-c", script] if script.contains("du -sck") => {
                    let mounts = args.windows(2).filter(|p| p[0] == "-v").map(|p| p[1].split(':').next().unwrap());
                    let need: usize = mounts.map(|m| w.vols.get(m).map_or(0, |v| v.data.len())).sum();
                    Ok(format!("{} {}", need.div_ceil(1024), w.free_kib.unwrap_or(1 << 30)))
                }
                ["run", ..] => {
                    let mounts: Vec<&str> = args.windows(2).filter(|p| p[0] == "-v").map(|p| p[1].split(':').next().unwrap()).collect();
                    if w.run_fails_part_way.as_deref().is_some_and(|m| line.contains(m)) {
                        w.run_fails_part_way = None;
                        let data = w.vols.get(mounts[0]).context("no such volume")?.data.clone();
                        w.vols.get_mut(mounts[1]).context("no such volume")?.data = data;
                        bail!("docker {line}: No space left on device (test)");
                    }
                    for pair in mounts.chunks(2) {
                        let data = w.vols.get(pair[0]).context("no such volume")?.data.clone();
                        w.vols.get_mut(pair[1]).context("no such volume")?.data = data;
                    }
                    Ok(String::new())
                }
                _ => bail!("fake docker: unexpected `{line}`"),
            }
        }

        fn snap_volumes(&self) -> Result<Vec<u8>> {
            let w = self.w.lock().unwrap();
            let vols: Vec<serde_json::Value> = w
                .vols
                .iter()
                .filter(|(_, v)| v.labels.contains_key(SNAP))
                .map(|(n, v)| serde_json::json!({"Name": n, "Labels": v.labels}))
                .collect();
            Ok(serde_json::to_vec(&serde_json::json!({"Volumes": vols}))?)
        }

        fn export(&self, volume: &str, w: &mut dyn Write) -> Result<()> {
            let data = self.w.lock().unwrap().vols.get(volume).context("no such volume")?.data.clone();
            w.write_all(&data)?;
            Ok(())
        }

        fn import(&self, volume: &str, r: &mut dyn Read) -> Result<()> {
            let mut b = Vec::new();
            r.read_to_end(&mut b)?;
            self.w.lock().unwrap().vols.get_mut(volume).context("no such volume")?.data = b;
            Ok(())
        }

        fn home(&self) -> &Path {
            &self.home
        }

        fn now(&self) -> u64 {
            self.now.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    type Target = fn() -> Result<(S3, String)>;
    const LOCAL: Option<Target> = None;

    fn to(m: &crate::s3::mock::Mock) -> Option<impl FnOnce() -> Result<(S3, String)>> {
        let s3 = m.s3.clone();
        Some(move || Ok((s3, "snaps".to_string())))
    }

    /// Saves change the process's signal handlers; one at a time keeps them consistent.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bytes(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())).collect()
    }

    #[test]
    fn names_follow_compose_and_volume_rules() {
        assert_eq!(normalize_project("My App.v2"), "myappv2");
        assert_eq!(normalize_project("_x-Y"), "x-y");
        assert_eq!(normalize_name("feature/login-2"), "feature-login-2");
        assert_eq!(normalize_name("//"), "");
        assert_eq!(snapshot_volume("shop", "seeded", "17abcd", "shop_pg"), "rekcod-snap.shop.seeded.17abcd.shop_pg");
        // A data volume's last part is a volume name, which starts with a letter or digit.
        assert_eq!(marker_volume("shop", "seeded", "17abcd"), "rekcod-snap-done.shop.seeded.17abcd");
        assert_ne!(marker_volume("shop", "seeded", "17abcd"), snapshot_volume("shop", "seeded", "17abcd", "done"));
        assert!(valid_volume("shop_pg") && valid_volume("a.b-c") && !valid_volume("../x") && !valid_volume("") && !valid_volume("a/b"));
    }

    #[test]
    fn the_highest_commit_wins_then_the_newest() {
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
            // A legacy snapshot, then a finished generation with an old-style marker name,
            // then a save that never finished.
            vol("rekcod-snap.shop.seeded.shop_pg", None, 10, serde_json::json!({SNAP_VOLUME: "shop_pg"})),
            vol("g20.pg", Some("20a"), 20, serde_json::json!({SNAP_VOLUME: "shop_pg"})),
            vol("rekcod-snap.shop.seeded.20a.done", Some("20a"), 20, serde_json::json!({SNAP_DONE: "1"})),
            vol("g30.pg", Some("30b"), 30, serde_json::json!({SNAP_VOLUME: "shop_pg", SNAP_RAW: "feature/x"})),
        ]});
        let s = parse(json.to_string().as_bytes());
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].created, s[0].generation.as_str(), s[0].commit), (20, "20a", 0));
        assert_eq!(s[0].volumes["shop_pg"], "g20.pg");
        let mut stale = s[0].stale.clone();
        stale.sort();
        assert_eq!(stale, ["g30.pg", "rekcod-snap.shop.seeded.shop_pg"]);

        // An older backup committed after a newer save is the snapshot.
        let json = serde_json::json!({"Volumes": [
            vol("new.pg", Some("50a"), 50, serde_json::json!({SNAP_VOLUME: "shop_pg"})),
            vol("new.done", Some("50a"), 50, serde_json::json!({SNAP_DONE: "1", SNAP_COMMIT: "1"})),
            vol("pulled.pg", Some("5b"), 5, serde_json::json!({SNAP_VOLUME: "shop_pg"})),
            vol("pulled.done", Some("5b"), 5, serde_json::json!({SNAP_DONE: "1", SNAP_COMMIT: "2"})),
        ]});
        let s = parse(json.to_string().as_bytes());
        assert_eq!((s[0].generation.as_str(), s[0].commit, s[0].created), ("5b", 2, 5));
    }

    #[test]
    fn saves_in_the_same_second_keep_the_last() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        // A finished generation from this same second whose id sorts after any new one, with
        // a marker from before commit numbers: by timestamp and id it would win.
        for (vol, extra) in [
            ("rekcod-snap.shop.seeded.1000zzzz.shop_pg", (SNAP_VOLUME, "shop_pg")),
            ("rekcod-snap.shop.seeded.1000zzzz.done", (SNAP_DONE, "1")),
        ] {
            let labels =
                [(SNAP, "1"), (SNAP_PROJECT, "shop"), (SNAP_NAME, "seeded"), (SNAP_GEN, "1000zzzz"), (SNAP_CREATED, "1000"), extra];
            let labels = labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            d.w.lock().unwrap().vols.insert(vol.into(), Vol { labels, data: b"old".to_vec() });
        }
        save_with(&d, Some("shop"), Some("seeded"), LOCAL).unwrap();
        assert_eq!(d.snapshot("shop", "seeded"), bytes(&[("shop_pg", "v1")]));
        d.write("shop_pg", b"v2");
        save_with(&d, Some("shop"), Some("seeded"), LOCAL).unwrap();
        assert_eq!(d.snapshot("shop", "seeded"), bytes(&[("shop_pg", "v2")]));
        let s = find(&d, "shop", "seeded").unwrap().unwrap();
        assert_eq!((s.commit, s.created, s.stale.len()), (2, 1000, 0));
        assert_eq!(d.names().len(), 3, "one generation and its marker: {:?}", d.names());
    }

    #[test]
    fn pulling_an_older_backup_restores_it() {
        let _serial = serial();
        let s3 = crate::s3::mock::start();
        let laptop = Fake::new(1000);
        laptop.volume("shop_pg", "shop", b"backup");
        save_with(&laptop, Some("shop"), Some("seeded"), to(&s3)).unwrap();

        // The other Mac saved `seeded` later, with other data.
        let desk = Fake::new(5000);
        desk.volume("shop_pg", "shop", b"local");
        save_with(&desk, Some("shop"), Some("seeded"), LOCAL).unwrap();
        desk.write("shop_pg", b"current");
        restore_with(&desk, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        assert_eq!(desk.data("shop_pg"), b"backup");
        assert_eq!(find(&desk, "shop", "seeded").unwrap().unwrap().created, 1000);
        assert_eq!(desk.snapshot("shop", PRE_RESTORE), bytes(&[("shop_pg", "current")]));
    }

    #[test]
    fn a_failed_push_leaves_the_last_one_pullable() {
        let _serial = serial();
        let s3 = crate::s3::mock::start();
        let d = Fake::new(1000);
        d.volume("shop_a", "shop", b"a1");
        d.volume("shop_b", "shop", b"b1");
        save_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        let first: Vec<String> = s3.keys();
        let pulled = || {
            let other = Fake::new(9000);
            other.volume("shop_a", "shop", b"");
            other.volume("shop_b", "shop", b"");
            restore_with(&other, Some("shop"), Some("seeded"), to(&s3)).unwrap();
            [other.data("shop_a"), other.data("shop_b")]
        };
        assert_eq!(pulled(), [b"a1".to_vec(), b"b1".to_vec()]);

        d.write("shop_a", b"a2");
        d.write("shop_b", b"b2");
        // Each upload in turn fails: the first archive, the second, the manifest.
        for (method, path) in [("PUT", "/shop_a.tar.zst"), ("PUT", "/shop_b.tar.zst"), ("PUT", "/manifest.json")] {
            s3.fail(method, path, 500);
            assert!(save_with(&d, Some("shop"), Some("seeded"), to(&s3)).is_err(), "{path}");
            assert_eq!(pulled(), [b"a1".to_vec(), b"b1".to_vec()], "after failing {path}");
            if path != "/manifest.json" {
                assert_eq!(s3.keys(), first, "the failed push's archives are deleted");
            }
        }
        d.now.store(10_000, std::sync::atomic::Ordering::Relaxed);
        save_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        assert_eq!(pulled(), [b"a2".to_vec(), b"b2".to_vec()]);
        // This push, the one it replaced, and the one that failed at the manifest: it isn't
        // an hour older than the replaced one, so it could be another Mac's push in flight.
        let generations = |keys: Vec<String>| {
            let mut g: Vec<String> =
                keys.iter().filter(|k| k.split('/').nth(5).is_some()).map(|k| k.split('/').nth(4).unwrap().to_string()).collect();
            g.dedup();
            g
        };
        assert_eq!(generations(s3.keys()).len(), 3, "{:?}", s3.keys());
        d.now.store(20_000, std::sync::atomic::Ordering::Relaxed);
        save_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        let kept = generations(s3.keys());
        assert_eq!(kept.len(), 2, "{:?}", s3.keys());
        assert!(!kept.iter().any(|g| first.iter().any(|k| k.contains(&format!("/{g}/")))), "the first push is pruned");
    }

    #[test]
    fn pull_rejects_a_changed_archive() {
        let _serial = serial();
        let s3 = crate::s3::mock::start();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"good");
        save_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        let other = Fake::new(2000);
        other.volume("shop_pg", "shop", b"mine");
        save_with(&other, Some("shop"), Some("seeded"), LOCAL).unwrap();
        {
            let mut st = s3.state.lock().unwrap();
            let key = st.objects.keys().find(|k| k.ends_with("/shop_pg.tar.zst")).unwrap().clone();
            st.objects.insert(key, zstd::encode_all(&b"evil"[..], 3).unwrap());
        }
        let e = restore_with(&other, Some("shop"), Some("seeded"), to(&s3)).unwrap_err();
        assert!(format!("{e:#}").contains("but the manifest says"), "{e:#}");
        assert_eq!(other.data("shop_pg"), b"mine");
        assert_eq!(other.snapshot("shop", "seeded"), bytes(&[("shop_pg", "mine")]));
        assert_eq!(other.names().len(), 3, "nothing of the pull is left: {:?}", other.names());
    }

    #[test]
    fn pull_reads_pushes_from_before_generations() {
        let _serial = serial();
        let s3 = crate::s3::mock::start();
        let manifest = serde_json::json!({"project": "shop", "name": "seeded", "created": 700,
            "volumes": {"shop_pg": {(PROJECT_LABEL): "shop"}}});
        s3.s3.put("snaps", "snaps/shop/seeded/manifest.json", Body::Bytes(manifest.to_string().as_bytes())).unwrap();
        let archive = zstd::encode_all(&b"v1 data"[..], 3).unwrap();
        s3.s3.put("snaps", "snaps/shop/seeded/shop_pg.tar.zst", Body::Bytes(&archive)).unwrap();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"");
        restore_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        assert_eq!(d.data("shop_pg"), b"v1 data");
        // The next push replaces the old layout, so an old rekcod can't pull stale archives.
        save_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        assert!(!s3.keys().contains(&"snaps/snaps/shop/seeded/shop_pg.tar.zst".to_string()), "{:?}", s3.keys());
    }

    #[test]
    fn a_failed_pause_unpauses_what_it_paused() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        save_with(&d, Some("shop"), Some("seeded"), LOCAL).unwrap();
        d.write("shop_pg", b"v2");
        for c in ["c1", "c2", "c3"] {
            d.container(c, "shop");
        }
        // c1 pauses, c2 doesn't; then the copy fails with all three paused.
        for fail in ["pause c2", "run "] {
            d.fail(fail);
            assert!(save_with(&d, Some("shop"), Some("seeded"), LOCAL).is_err(), "{fail}");
            assert_eq!([d.state("c1"), d.state("c2"), d.state("c3")], ["running"; 3], "{fail}");
            assert_eq!(d.snapshot("shop", "seeded"), bytes(&[("shop_pg", "v1")]), "{fail}");
            assert_eq!(d.names().len(), 3, "{fail}: {:?}", d.names());
        }
    }

    #[test]
    fn a_failed_stop_restarts_what_it_stopped() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        save_with(&d, Some("shop"), Some("seeded"), LOCAL).unwrap();
        d.write("shop_pg", b"v2");
        for c in ["c1", "c2"] {
            d.container(c, "shop");
        }
        d.w.lock().unwrap().stop_fails_after_first = true;
        assert!(restore_with(&d, Some("shop"), Some("seeded"), LOCAL).is_err());
        assert_eq!([d.state("c1"), d.state("c2")], ["running"; 2]);
        assert_eq!(d.data("shop_pg"), b"v2");
    }

    #[test]
    fn a_volume_named_done_is_saved() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("done", "audit", b"seed data");
        save_with(&d, Some("audit"), Some("seed"), LOCAL).unwrap();
        assert_eq!(d.snapshot("audit", "seed"), bytes(&[("done", "seed data")]));
        d.write("done", b"changed");
        restore_with(&d, Some("audit"), Some("seed"), LOCAL).unwrap();
        assert_eq!(d.data("done"), b"seed data");
    }

    #[test]
    fn a_save_whose_marker_is_wrong_fails_and_keeps_the_last() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        save_with(&d, Some("shop"), Some("seeded"), LOCAL).unwrap();
        let before = d.names();
        d.write("shop_pg", b"v2");
        d.w.lock().unwrap().unlabelled_markers = true;
        let e = save_with(&d, Some("shop"), Some("seeded"), LOCAL).unwrap_err();
        assert!(e.to_string().contains("wasn't saved"), "{e:#}");
        assert_eq!(d.snapshot("shop", "seeded"), bytes(&[("shop_pg", "v1")]));
        assert_eq!(d.names(), before);
    }

    #[test]
    fn changes_to_one_snapshot_take_turns() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        let held = lock(&d, "shop", "seeded").unwrap();
        std::thread::scope(|s| {
            let save = s.spawn(|| save_with(&d, Some("shop"), Some("seeded"), LOCAL));
            std::thread::sleep(std::time::Duration::from_millis(300));
            assert!(find(&d, "shop", "seeded").unwrap().is_none(), "saved while another change held the lock");
            // Another snapshot's lock is separate.
            drop(lock(&d, "shop", "other").unwrap());
            drop(held);
            save.join().unwrap().unwrap();
        });
        assert_eq!(d.snapshot("shop", "seeded"), bytes(&[("shop_pg", "v1")]));
    }

    #[test]
    fn saves_and_restores_of_one_project_take_turns() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        d.container("c1", "shop");
        save_with(&d, Some("shop"), Some("b"), LOCAL).unwrap();
        d.write("shop_pg", b"v2");
        // A save of another name holds the project.
        let held = lock_project(&d, "shop").unwrap();
        let before = d.w.lock().unwrap().log.len();
        std::thread::scope(|s| {
            let restore = s.spawn(|| restore_with(&d, Some("shop"), Some("b"), LOCAL));
            std::thread::sleep(std::time::Duration::from_millis(300));
            let log = d.w.lock().unwrap().log[before..].to_vec();
            assert!(!log.iter().any(|l| l.starts_with("stop") || l.starts_with("run") || l.starts_with("pause")), "{log:?}");
            drop(held);
            restore.join().unwrap().unwrap();
        });
        assert_eq!(d.data("shop_pg"), b"v1");
    }

    #[test]
    fn restore_stops_restarting_containers_and_refuses_paused_ones() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        save_with(&d, Some("shop"), Some("b"), LOCAL).unwrap();
        d.write("shop_pg", b"v2");
        d.container("c1", "shop");
        d.w.lock().unwrap().containers.insert("c2".into(), ("shop".into(), "paused"));
        let e = restore_with(&d, Some("shop"), Some("b"), LOCAL).unwrap_err();
        assert!(e.to_string().contains("c2 of shop is paused"), "{e:#}");
        assert_eq!((d.data("shop_pg"), d.state("c1")), (b"v2".to_vec(), "running"));

        d.w.lock().unwrap().containers.insert("c2".into(), ("shop".into(), "restarting"));
        // A save can't pause it, so it refuses too.
        assert!(save_with(&d, Some("shop"), Some("x"), LOCAL).unwrap_err().to_string().contains("restarting"));
        let before = d.w.lock().unwrap().log.len();
        restore_with(&d, Some("shop"), Some("b"), LOCAL).unwrap();
        let log = d.w.lock().unwrap().log[before..].to_vec();
        assert!(log.iter().any(|l| l == "stop c1 c2"), "{log:?}");
        assert_eq!(d.data("shop_pg"), b"v1");
        assert_eq!([d.state("c1"), d.state("c2")], ["running"; 2]);
    }

    #[test]
    fn a_restore_without_room_stops_nothing() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", &vec![1u8; 600 * 1024]);
        d.container("c1", "shop");
        save_with(&d, Some("shop"), Some("big"), LOCAL).unwrap();
        // 600 KiB live plus 600 KiB of snapshot, and 1000 KiB free.
        d.w.lock().unwrap().free_kib = Some(1000);
        let e = restore_with(&d, Some("shop"), Some("big"), LOCAL).unwrap_err();
        assert!(e.to_string().contains("1000 KiB") || e.to_string().contains("MiB are free"), "{e:#}");
        assert_eq!(d.state("c1"), "running");
        d.w.lock().unwrap().free_kib = Some(1400);
        restore_with(&d, Some("shop"), Some("big"), LOCAL).unwrap();
    }

    #[test]
    fn a_restore_that_fails_part_way_puts_the_volumes_back() {
        let _serial = serial();
        let d = Fake::new(1000);
        d.volume("shop_a", "shop", b"a1");
        d.volume("shop_b", "shop", b"b1");
        d.container("c1", "shop");
        save_with(&d, Some("shop"), Some("good"), LOCAL).unwrap();
        d.write("shop_a", b"a2");
        d.write("shop_b", b"b2");
        d.w.lock().unwrap().run_fails_part_way = Some("rekcod-snap.shop.good.".into());
        let e = restore_with(&d, Some("shop"), Some("good"), LOCAL).unwrap_err();
        assert!(format!("{e:#}").contains("back as they were"), "{e:#}");
        assert_eq!([d.data("shop_a"), d.data("shop_b")], [b"a2".to_vec(), b"b2".to_vec()]);
        assert_eq!(d.state("c1"), "running");

        // Copying back fails too: nothing starts on half-copied volumes.
        d.w.lock().unwrap().run_fails_part_way = Some("rekcod-snap.shop.good.".into());
        d.fail("run --rm --network none -v rekcod-snap.shop.pre-restore.");
        let e = restore_with(&d, Some("shop"), Some("good"), LOCAL).unwrap_err();
        assert!(format!("{e:#}").contains("rekcod snap restore pre-restore -p shop"), "{e:#}");
        assert_eq!(d.state("c1"), "exited");
    }

    #[test]
    fn a_pulled_branch_snapshot_saves_again() {
        let _serial = serial();
        let s3 = crate::s3::mock::start();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        save_with(&d, Some("shop"), Some("feature/x"), to(&s3)).unwrap();
        let other = Fake::new(2000);
        other.volume("shop_pg", "shop", b"");
        restore_with(&other, Some("shop"), Some("feature/x"), to(&s3)).unwrap();
        // The default save on that branch on the other Mac.
        other.write("shop_pg", b"v2");
        save_with(&other, Some("shop"), Some("feature/x"), LOCAL).unwrap();
        assert_eq!(other.snapshot("shop", "feature-x"), bytes(&[("shop_pg", "v2")]));
    }

    #[test]
    fn a_push_that_lands_late_keeps_its_archives() {
        let _serial = serial();
        let s3 = crate::s3::mock::start();
        let d = Fake::new(1000);
        d.volume("shop_pg", "shop", b"v1");
        save_with(&d, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        // This Mac uploads its archives...
        d.now.store(10_000, std::sync::atomic::Ordering::Relaxed);
        d.write("shop_pg", b"v2");
        let a = save_as(&d, "shop", "seeded", None).unwrap();
        let staged = upload(&d, &s3.s3, "snaps", "shop", "seeded", &a).unwrap();
        // ...another Mac pushes the same snapshot from start to end...
        let other = Fake::new(10_001);
        other.volume("shop_pg", "shop", b"v3");
        save_with(&other, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        // ...and this one's manifest is written last.
        commit(&s3.s3, "snaps", "shop", "seeded", &a, staged).unwrap();
        let third = Fake::new(20_000);
        third.volume("shop_pg", "shop", b"");
        restore_with(&third, Some("shop"), Some("seeded"), to(&s3)).unwrap();
        assert_eq!(third.data("shop_pg"), b"v2");
        let gens: std::collections::BTreeSet<String> =
            s3.keys().iter().filter_map(|k| k.split('/').nth(4).filter(|_| k.split('/').nth(5).is_some()).map(String::from)).collect();
        assert_eq!(gens.len(), 2, "the first push is pruned, the two recent ones kept: {:?}", s3.keys());
    }

    #[test]
    fn temp_archives_of_dead_processes_are_removed() {
        let d = Fake::new(1000);
        let dir = d.home().join("run/snap/tmp");
        std::fs::create_dir_all(&dir).unwrap();
        // macOS pids stop at 99998.
        std::fs::write(dir.join("snap-push-999999.tar.zst"), b"x").unwrap();
        let mine = dir.join(format!("snap-pull-{}.tar.zst", std::process::id()));
        std::fs::write(&mine, b"x").unwrap();
        tmp_archive(&d, "push").unwrap();
        assert!(!dir.join("snap-push-999999.tar.zst").exists());
        assert!(mine.exists(), "a live process's file stays");
    }
}
