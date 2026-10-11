//! Mac file changes -> the guest's caches and inotify.
//!
//! virtio-fs never tells the guest when the Mac changes a file. We watch with
//! FSEvents and forward changed paths to the agent:
//! - Guest with the patched fuse module (`fs-notify`, phase 2): the guest caches
//!   lookups/attributes/pages for a long time, so *every* shared path the guest
//!   may have cached must be forwarded. `/Users` (minus `~/Library`) and
//!   `/Volumes` are watched from boot; other bind sources (e.g. `/private/tmp`)
//!   while a container uses them, with a subtree invalidation when the watch starts.
//! - Stock guest (phase 1 fallback): only running containers' bind sources, so
//!   watchers fire (the agent re-chmods the path).
//!
//! FSEvents flags are sticky and the fallback chmod produces events too, so a
//! path is only forwarded when its (mtime, size, inode) differs from last time.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rekcod_proto::notify_line;
use serde::Deserialize;
use tokio::sync::mpsc;

/// Host directories that exist at the same path in the guest.
pub const SHARED_ROOTS: [&str; 3] = ["/Users", "/Volumes", "/private"];
/// Watched from boot when the guest caches (fs-notify).
pub const GLOBAL_ROOTS: [&str; 2] = ["/Users", "/Volumes"];

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "Mounts", default)]
    mounts: Option<Vec<Mount>>,
}

#[derive(Deserialize)]
struct Mount {
    #[serde(rename = "Type", default)]
    kind: String,
    #[serde(rename = "Source", default)]
    source: String,
}

/// Bind-mount sources (under the shared roots) from `GET /containers/json`,
/// with paths nested inside another source dropped.
pub fn bind_sources(containers_json: &[u8]) -> serde_json::Result<BTreeSet<PathBuf>> {
    let containers: Vec<Container> = serde_json::from_slice(containers_json)?;
    let all: BTreeSet<PathBuf> = containers
        .into_iter()
        .flat_map(|c| c.mounts.unwrap_or_default())
        .filter(|m| m.kind == "bind")
        .map(|m| mac_path(Path::new(&m.source)))
        .filter(|p| SHARED_ROOTS.iter().any(|r| p.starts_with(r)))
        .collect();
    Ok(outermost(all))
}

/// The Mac path FSEvents reports for a bind source: `/tmp` and `/var/folders`
/// are symlinks into `/private` on macOS (the guest mounts them the same way).
pub fn mac_path(source: &Path) -> PathBuf {
    for alias in ["/tmp", "/var/folders"] {
        if let Ok(rest) = source.strip_prefix(alias) {
            return Path::new("/private").join(alias.trim_start_matches('/')).join(rest);
        }
    }
    source.to_path_buf()
}

fn outermost(all: BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    all.iter().filter(|p| !all.iter().any(|q| q != *p && p.starts_with(q))).cloned().collect()
}

/// Busy per-user directories no container mounts in practice: `~/Library`
/// (app caches, browsers) and rekcod's own state (VM disks, logs).
pub fn excluded(path: &Path) -> bool {
    let mut parts = path.components();
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(Component::RootDir), Some(Component::Normal(users)), Some(Component::Normal(_)), Some(Component::Normal(dir)))
            if users == "Users" && (dir == "Library" || dir == ".rekcod")
    )
}

/// (size, mtime s, mtime ns, ctime s, ctime ns, inode); `None` = deleted.
type Fingerprint = Option<[i64; 6]>;

fn fingerprint(path: &Path) -> Fingerprint {
    let m = std::fs::symlink_metadata(path).ok()?;
    Some([m.size() as i64, m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec(), m.ino() as i64])
}

/// What decides "changed". The phase-1 fallback re-chmods paths in the guest,
/// which bumps ctime on the Mac, so ctime must not count there (feedback loop).
fn change_key(fp: &Fingerprint, with_ctime: bool) -> Fingerprint {
    fp.map(|f| if with_ctime { f } else { [f[0], f[1], f[2], 0, 0, f[5]] })
}

enum Change {
    Path(PathBuf),
    /// FSEvents lost track (or a watch just started): everything below is suspect.
    Tree(PathBuf),
}

pub struct Watch {
    watcher: Mutex<RecommendedWatcher>,
    /// Watched paths: the global roots (if any) plus current bind sources.
    roots: Mutex<BTreeSet<PathBuf>>,
    global: bool,
    tx: mpsc::UnboundedSender<Change>,
}

impl Watch {
    /// Starts the FSEvents watcher; changed paths are delivered in batches to
    /// `forward` (a trailing `/` marks a subtree). `global`: the guest caches,
    /// so watch [`GLOBAL_ROOTS`] from now on.
    pub fn start<F, Fut>(global: bool, forward: F) -> notify::Result<Arc<Watch>>
    where
        F: Fn(Vec<String>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Change>();
        let events = tx.clone();
        // Test hook (tests/stress/overflow.sh): treat events below this path as
        // if FSEvents had dropped them, i.e. deliver the parent as a subtree.
        let force_rescan = std::env::var_os("REKCOD_TEST_FORCE_RESCAN").map(PathBuf::from);
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            let rescan = event.need_rescan();
            for path in event.paths {
                if excluded(&path) {
                    continue;
                }
                let change = match &force_rescan {
                    Some(prefix) if path.starts_with(prefix) => Change::Tree(path.parent().unwrap_or(&path).to_path_buf()),
                    _ if rescan => Change::Tree(path),
                    _ => Change::Path(path),
                };
                let _ = events.send(change);
            }
        })?;
        tokio::spawn(async move {
            let mut seen: HashMap<PathBuf, Fingerprint> = HashMap::new();
            while let Some(first) = rx.recv().await {
                // Collect a short burst (editors and git write several files at once).
                let mut batch = vec![first];
                let window = tokio::time::sleep(Duration::from_millis(10));
                tokio::pin!(window);
                loop {
                    tokio::select! {
                        _ = &mut window => break,
                        Some(c) = rx.recv() => batch.push(c),
                    }
                }
                if seen.len() > 200_000 {
                    seen.clear();
                }
                let mut changed = BTreeSet::new();
                for change in batch {
                    let line = match change {
                        Change::Tree(path) => path.to_str().map(|p| format!("{}/", p.trim_end_matches('/'))),
                        Change::Path(path) => {
                            let fp = fingerprint(&path);
                            let key = change_key(&fp, global);
                            if seen.get(&path) == Some(&key) {
                                continue;
                            }
                            seen.insert(path.clone(), key);
                            let stat = fp.map(|f| [f[0], f[1], f[2], f[3], f[4]]);
                            path.to_str().filter(|p| !p.contains('\t')).map(|p| notify_line(p, stat))
                        }
                    };
                    if let Some(line) = line.filter(|l| !l.contains('\n')) {
                        changed.insert(line);
                    }
                }
                if !changed.is_empty() {
                    forward(changed.into_iter().collect()).await;
                }
            }
        });
        let watch = Arc::new(Watch { watcher: Mutex::new(watcher), roots: Mutex::default(), global, tx });
        if global {
            watch.set_roots(BTreeSet::new());
        }
        Ok(watch)
    }

    /// Watches the global roots (if enabled) plus `bind_sources` not already
    /// covered by them; files are watched via their directory. Returns log lines.
    pub fn set_roots(&self, bind_sources: BTreeSet<PathBuf>) -> Vec<String> {
        let globals: BTreeSet<PathBuf> = if self.global {
            GLOBAL_ROOTS.iter().map(PathBuf::from).filter(|p| p.is_dir()).collect()
        } else {
            BTreeSet::new()
        };
        let mut wanted: BTreeSet<PathBuf> = bind_sources
            .into_iter()
            .filter_map(|p| if p.is_file() { p.parent().map(Path::to_path_buf) } else { Some(p) })
            .filter(|p| !globals.iter().any(|g| p.starts_with(g)))
            .collect();
        wanted.extend(globals);
        let wanted = outermost(wanted);

        let mut roots = self.roots.lock().unwrap();
        let mut watcher = self.watcher.lock().unwrap();
        let mut log = Vec::new();
        for gone in roots.difference(&wanted) {
            let _ = watcher.unwatch(gone);
        }
        for new in wanted.difference(&roots) {
            match watcher.watch(new, RecursiveMode::Recursive) {
                Ok(()) => {
                    log.push(format!("watching {}", new.display()));
                    // Changes made while unwatched were missed; drop what the guest cached.
                    if self.global {
                        let _ = self.tx.send(Change::Tree(new.clone()));
                    }
                }
                Err(e) => log.push(format!("watch {}: {e}", new.display())),
            }
        }
        *roots = wanted;
        log
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_sources_keeps_outermost_shared_binds() {
        let json = br#"[
            {"Mounts":[{"Type":"bind","Source":"/Users/me/app","Destination":"/app"},
                       {"Type":"bind","Source":"/Users/me/app/www","Destination":"/www"},
                       {"Type":"volume","Source":"/var/lib/docker/volumes/x/_data","Destination":"/data"},
                       {"Type":"bind","Source":"/var/run/docker.sock","Destination":"/var/run/docker.sock"}]},
            {"Mounts":[{"Type":"bind","Source":"/private/var/folders/x/T/proj","Destination":"/p"}]},
            {"Mounts":[{"Type":"bind","Source":"/tmp/scratch","Destination":"/s"}]},
            {"Mounts":[{"Type":"bind","Source":"/tmpfoo","Destination":"/s"}]},
            {"Mounts":null}
        ]"#;
        let got: Vec<String> = bind_sources(json).unwrap().iter().map(|p| p.display().to_string()).collect();
        assert_eq!(got, vec!["/Users/me/app", "/private/tmp/scratch", "/private/var/folders/x/T/proj"]);
    }

    #[test]
    fn excludes_library_and_rekcod_state_only() {
        assert!(excluded(Path::new("/Users/me/Library/Caches/x")));
        assert!(excluded(Path::new("/Users/me/.rekcod/vm/data.img")));
        assert!(!excluded(Path::new("/Users/me/code/Library/x.ts")));
        assert!(!excluded(Path::new("/Users/me/code/app.ts")));
        assert!(!excluded(Path::new("/Volumes/ext/Library/x")));
    }

    #[test]
    fn fingerprint_changes_on_write_not_on_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rekcod-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one").unwrap();
        let before = fingerprint(&f);
        let mode = std::fs::metadata(&f).unwrap().permissions().mode();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            change_key(&fingerprint(&f), false),
            change_key(&before, false),
            "fallback: same-mode chmod must not look like a change"
        );
        assert_ne!(fingerprint(&f), before, "with ctime (caching guest), chmod is a change");
        std::fs::write(&f, "two!").unwrap();
        assert_ne!(fingerprint(&f), before);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(fingerprint(&f), None);
    }
}
