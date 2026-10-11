//! Where rekcod keeps things: `$REKCOD_HOME`, default `~/.rekcod`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

#[derive(Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Paths> {
        let home = match std::env::var_os("REKCOD_HOME") {
            Some(h) => PathBuf::from(h),
            None => PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join(".rekcod"),
        };
        for dir in ["run", "logs", "vm"] {
            std::fs::create_dir_all(home.join(dir))?;
        }
        Ok(Paths { home })
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("run/docker.sock")
    }
    pub fn pid_file(&self) -> PathBuf {
        self.home.join("run/daemon.pid")
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
    pub fn guest_version(&self) -> PathBuf {
        self.home.join("vm/guest-version")
    }

    pub fn docker_host(&self) -> String {
        format!("unix://{}", self.socket().display())
    }

    /// Guest kernel, initramfs and root image: `$REKCOD_GUEST_DIR`, then
    /// `~/.rekcod/guest`, then next to the binary (`../share/rekcod/guest`),
    /// then the repo's `out/guest` for dev builds.
    pub fn guest_dir(&self) -> Result<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(dir) = std::env::var_os("REKCOD_GUEST_DIR") {
            candidates.push(PathBuf::from(dir));
        }
        candidates.push(self.home.join("guest"));
        if let Some(bin_dir) = std::env::current_exe()?.canonicalize()?.parent() {
            candidates.push(bin_dir.join("../share/rekcod/guest"));
            candidates.push(bin_dir.join("../../out/guest"));
        }
        for dir in &candidates {
            if dir.join("Image").exists() && dir.join("root.img").exists() {
                return Ok(dir.canonicalize()?);
            }
        }
        bail!(
            "guest image not found (looked in {}); build it with scripts/build.sh",
            candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(", ")
        )
    }
}
