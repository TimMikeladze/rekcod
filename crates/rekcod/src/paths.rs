//! Where rekcod keeps things: `$REKCOD_HOME`, default `~/.rekcod`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// Runs `f` with the process umask set to `mask`. The umask is process-wide, so keep `f`
/// short (a bind or a create).
pub fn with_umask<T>(mask: libc::mode_t, f: impl FnOnce() -> T) -> T {
    let old = unsafe { libc::umask(mask) };
    let r = f();
    unsafe { libc::umask(old) };
    r
}

/// An exclusive `flock` on `path` (created if missing), held until the file is dropped.
pub fn lock(path: &std::path::Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("lock {}", path.display()));
    }
    Ok(f)
}

/// Whether some process holds an `flock` on `path`.
pub fn locked(path: &std::path::Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(f) = std::fs::File::open(path) else { return false };
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
}

/// Takes away whatever `path`'s mode allows beyond `mode`.
pub fn restrict(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if m & !mode != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(m & mode))?;
    }
    Ok(())
}

/// Writes a file only this user can read, created 0600 rather than chmodded after.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Data disk size; sparse, so only used space costs anything on the Mac.
pub const DEFAULT_DISK_GIB: u64 = 256;

/// `$REKCOD_HOME`, default `~/.rekcod`, without creating anything.
pub fn home_dir() -> Result<PathBuf> {
    Ok(match std::env::var_os("REKCOD_HOME") {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join(".rekcod"),
    })
}

#[derive(Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Paths> {
        Paths::at(home_dir()?)
    }

    /// Another rekcod home (the sandbox VM's, `~/.rekcod/sandbox`).
    ///
    /// The home and its directories are 0700 and the VM disks 0600, whatever the umask, and
    /// an existing install is brought there too: the data disk holds every volume and
    /// container, and sockets in run/ reach dockerd (root in the VM, with /Users mounted).
    /// Modes that can't be changed are left for `rekcod doctor` to report.
    pub fn at(home: PathBuf) -> Result<Paths> {
        use std::os::unix::fs::DirBuilderExt;
        for dir in ["run", "logs", "vm"] {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(home.join(dir))?;
        }
        let paths = Paths { home };
        for (path, mode) in paths.private() {
            if path.exists() {
                let _ = restrict(&path, mode);
            }
        }
        Ok(paths)
    }

    /// What only this user may reach, and the mode it should have.
    fn private(&self) -> [(PathBuf, u32); 6] {
        let dir = |d: &str| (self.home.join(d), 0o700);
        [(self.home.clone(), 0o700), dir("run"), dir("logs"), dir("vm"), (self.root_disk(), 0o600), (self.data_disk(), 0o600)]
    }

    /// `path mode` for each of [`Paths::private`] that others can reach (`rekcod doctor`).
    pub fn not_private(&self) -> Vec<String> {
        use std::os::unix::fs::PermissionsExt;
        self.private()
            .into_iter()
            .filter_map(|(path, mode)| {
                let m = std::fs::metadata(&path).ok()?.permissions().mode() & 0o777;
                (m & !mode != 0).then(|| format!("{} {m:o}", path.display()))
            })
            .collect()
    }

    /// True for `~/.rekcod`. Other homes (tests, benches) must not repoint the shared
    /// `rekcod` docker context unless asked to switch to it.
    pub fn is_default_home(&self) -> bool {
        std::env::var_os("HOME").is_some_and(|h| PathBuf::from(h).join(".rekcod") == self.home)
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("run/docker.sock")
    }
    pub fn control_socket(&self) -> PathBuf {
        self.home.join("run/control.sock")
    }
    pub fn pid_file(&self) -> PathBuf {
        self.home.join("run/daemon.pid")
    }

    /// Locks the pid file for this process's lifetime (keep the returned file) and writes
    /// our pid into it. `running_pid` only trusts a locked pid file, so one left behind by
    /// a crash, whose pid the OS may since have given to another process, never counts.
    /// Fails if another process holds it.
    pub fn hold_pid_file(&self) -> Result<std::fs::File> {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let mut f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.pid_file())?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("another rekcod daemon is running for {}", self.home.display());
        }
        f.set_len(0)?;
        write!(f, "{}", std::process::id())?;
        Ok(f)
    }
    /// Held while a CLI checks for, spawns and waits on the daemon, so two commands run at
    /// once (the menu bar and a terminal, say) never boot two VMs on one disk.
    pub fn boot_lock(&self) -> PathBuf {
        self.home.join("run/boot.lock")
    }
    /// Written when `data.img` is created; tells the guest a blank disk may be formatted.
    pub fn data_fresh(&self) -> PathBuf {
        self.home.join("vm/data.fresh")
    }
    /// `clean` after the daemon stopped on purpose; anything else means it crashed.
    pub fn last_exit(&self) -> PathBuf {
        self.home.join("run/last-exit")
    }
    /// `rekcod start` settings (cpus, memory, disk) that later boots reuse.
    pub fn config(&self) -> PathBuf {
        self.home.join("config.json")
    }
    pub fn status_file(&self) -> PathBuf {
        self.home.join("run/status.json")
    }
    pub fn prev_context(&self) -> PathBuf {
        self.home.join("run/prev-context")
    }
    pub fn daemon_log(&self) -> PathBuf {
        self.home.join("logs/daemon.log")
    }
    pub fn console_log(&self) -> PathBuf {
        self.home.join("logs/console.log")
    }
    pub fn root_disk(&self) -> PathBuf {
        self.home.join("vm/root.img")
    }
    pub fn data_disk(&self) -> PathBuf {
        self.home.join("vm/data.img")
    }
    pub fn mac_address(&self) -> PathBuf {
        self.home.join("vm/mac-address")
    }
    /// Recently bind-mounted Mac directories, most recent first; warmed at boot.
    pub fn warm_paths(&self) -> PathBuf {
        self.home.join("vm/warm-paths")
    }
    pub fn guest_version(&self) -> PathBuf {
        self.home.join("vm/guest-version")
    }

    pub fn docker_host(&self) -> String {
        format!("unix://{}", self.socket().display())
    }

    /// Guest kernel, initramfs and root image: `$REKCOD_GUEST_DIR`, then
    /// `~/.rekcod/guest`, then next to the binary (`../share/rekcod/guest`),
    /// then the repo's `out/guest` for dev builds, then (release builds) the
    /// embedded guest version under `~/.rekcod/guest/<version>`, downloaded if missing.
    pub fn guest_dir(&self) -> Result<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(dir) = std::env::var_os("REKCOD_GUEST_DIR") {
            candidates.push(PathBuf::from(dir));
        }
        // A release build boots the guest it was built with; a leftover dev guest in
        // ~/.rekcod/guest must not win over it. Dev builds (no pinned guest) look there.
        #[cfg(target_os = "macos")]
        let pinned = crate::fetch::GUEST_VERSION.is_some();
        #[cfg(not(target_os = "macos"))]
        let pinned = false;
        if !pinned {
            candidates.push(self.home.join("guest"));
        }
        if let Some(bin_dir) = std::env::current_exe()?.canonicalize()?.parent() {
            candidates.push(bin_dir.join("../share/rekcod/guest"));
            if !pinned {
                candidates.push(bin_dir.join("../../out/guest"));
            }
        }
        for dir in &candidates {
            if dir.join("Image").exists() && dir.join("root.img").exists() {
                return Ok(dir.canonicalize()?);
            }
        }
        // Release builds: the guest they were built with, downloaded on first use.
        #[cfg(target_os = "macos")]
        if let Some(dir) = crate::fetch::ensure_guest(&self.home)? {
            return Ok(dir);
        }
        bail!(
            "guest image not found (looked in {}); build it with scripts/build.sh",
            candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_private_under_a_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let chmod = |p: &std::path::Path, m| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        let tmp = std::env::temp_dir().join(format!("rekcod-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let dirs = ["", "run", "logs", "vm"].map(|d| home.join(d));

        let paths = with_umask(0o022, || Paths::at(home.clone())).unwrap();
        assert_eq!(dirs.each_ref().map(|d| mode(d)), [0o700; 4]);
        assert_eq!(paths.not_private(), Vec::<String>::new());

        // An install from before: everything readable by other accounts.
        for d in &dirs {
            chmod(d, 0o755);
        }
        for disk in [paths.root_disk(), paths.data_disk()] {
            std::fs::write(&disk, b"").unwrap();
            chmod(&disk, 0o644);
        }
        assert_eq!(paths.not_private().len(), 6);
        with_umask(0o022, || Paths::at(home.clone())).unwrap();
        assert_eq!(dirs.each_ref().map(|d| mode(d)), [0o700; 4]);
        assert_eq!((mode(&paths.root_disk()), mode(&paths.data_disk())), (0o600, 0o600));
        assert!(paths.not_private().is_empty());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn only_home_dot_rekcod_is_default() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert!(Paths { home: home.join(".rekcod") }.is_default_home());
        assert!(!Paths { home: home.join(".rkab") }.is_default_home());
    }
}
