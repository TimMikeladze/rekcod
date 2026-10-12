//! The default socket (`/var/run/docker.sock`) for clients that ignore docker
//! contexts (docs/default-socket.md).
//!
//! `/var/run/docker.sock -> ~/.rekcod/default.sock -> <engine socket>`: root sets
//! the first link once (`rekcod socket install`); start/stop retarget the second.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::Paths;

fn system_socket() -> PathBuf {
    std::env::var_os("REKCOD_SYSTEM_SOCKET").map(PathBuf::from).unwrap_or_else(|| "/var/run/docker.sock".into())
}

fn default_link(paths: &Paths) -> PathBuf {
    paths.home.join("default.sock")
}

/// What `/var/run/docker.sock` pointed at before install.
fn original_file(paths: &Paths) -> PathBuf {
    paths.home.join("default.sock.original")
}

fn prev_file(paths: &Paths) -> PathBuf {
    paths.home.join("run/prev-default-sock")
}

/// True when `/var/run/docker.sock` goes through `~/.rekcod/default.sock`.
pub fn installed(paths: &Paths) -> bool {
    std::fs::read_link(system_socket()).is_ok_and(|t| t == default_link(paths))
}

/// Atomically point the user-owned link at `target`.
fn retarget(link: &Path, target: &Path) -> Result<()> {
    let tmp = link.with_extension("sock.tmp");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).with_context(|| format!("link {}", tmp.display()))?;
    std::fs::rename(&tmp, link).with_context(|| format!("replace {}", link.display()))
}

/// `rekcod start`: route the default socket to rekcod. Returns false if not installed.
pub fn on_start(paths: &Paths) -> Result<bool> {
    if !installed(paths) {
        return Ok(false);
    }
    let link = default_link(paths);
    let current = std::fs::read_link(&link).ok();
    if current.as_deref() != Some(paths.socket().as_path()) {
        if let Some(c) = &current {
            std::fs::write(prev_file(paths), c.as_os_str().as_encoded_bytes())?;
        }
        retarget(&link, &paths.socket())?;
    }
    Ok(true)
}

/// `rekcod stop`: give the default socket back to whatever had it before.
pub fn on_stop(paths: &Paths) -> Result<()> {
    let link = default_link(paths);
    if std::fs::read_link(&link).ok().as_deref() != Some(paths.socket().as_path()) {
        return Ok(());
    }
    let prev = std::fs::read_to_string(prev_file(paths))
        .or_else(|_| std::fs::read_to_string(original_file(paths)))
        .unwrap_or_default();
    let prev = prev.trim();
    if prev.is_empty() {
        std::fs::remove_file(&link)?; // nothing else ever had it
    } else {
        retarget(&link, Path::new(prev))?;
    }
    Ok(())
}

fn sudo(args: &[&str]) -> Result<()> {
    let ok = Command::new("sudo").args(args).status().context("run sudo")?.success();
    if !ok {
        bail!("`sudo {}` failed", args.join(" "));
    }
    Ok(())
}

pub fn install(paths: &Paths, running: bool) -> Result<()> {
    let sys = system_socket();
    let link = default_link(paths);
    if installed(paths) {
        println!("{} already goes through rekcod ({})", sys.display(), link.display());
    } else {
        let original = std::fs::read_link(&sys).ok().filter(|t| t != &link);
        let original = original.map(|o| o.display().to_string()).unwrap_or_default();
        std::fs::write(original_file(paths), &original)?;
        std::fs::write(prev_file(paths), &original)?;
        match original.as_str() {
            "" => {
                let _ = std::fs::remove_file(&link);
            }
            o => retarget(&link, Path::new(o))?,
        }
        println!("pointing {} at {} (needs your password once)", sys.display(), link.display());
        let (sys_s, link_s) = (sys.display().to_string(), link.display().to_string());
        if !writable(&sys) {
            sudo(&["ln", "-sfn", &link_s, &sys_s])?;
        } else {
            std::fs::remove_file(&sys).ok();
            std::os::unix::fs::symlink(&link, &sys)?;
        }
    }
    if running {
        on_start(paths)?;
        println!("default socket: rekcod (`rekcod stop` hands it back)");
    } else {
        println!("default socket: switches to rekcod on `rekcod start`");
    }
    Ok(())
}

/// Can we replace `path` without root (tests point it at a temp dir)?
fn writable(path: &Path) -> bool {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap_or_default();
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

pub fn uninstall(paths: &Paths) -> Result<()> {
    let sys = system_socket();
    if !installed(paths) {
        println!("{} does not go through rekcod; nothing to do", sys.display());
        return Ok(());
    }
    let original = std::fs::read_to_string(original_file(paths)).unwrap_or_default();
    let original = original.trim();
    let sys_s = sys.display().to_string();
    let as_root = !writable(&sys);
    match (original.is_empty(), as_root) {
        (true, true) => sudo(&["rm", "-f", &sys_s])?,
        (false, true) => sudo(&["ln", "-sfn", original, &sys_s])?,
        (true, false) => std::fs::remove_file(&sys)?,
        (false, false) => retarget(&sys, Path::new(original))?,
    }
    for f in [default_link(paths), original_file(paths), prev_file(paths)] {
        let _ = std::fs::remove_file(f);
    }
    println!("{} restored{}", sys.display(), if original.is_empty() { " (removed)".into() } else { format!(" -> {original}") });
    Ok(())
}

/// One word for `rekcod status`.
pub fn state(paths: &Paths) -> &'static str {
    if !installed(paths) {
        "not installed (`rekcod socket install`)"
    } else if std::fs::read_link(default_link(paths)).ok().as_deref() == Some(paths.socket().as_path()) {
        "rekcod"
    } else {
        "another engine"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_start_stop_uninstall_round_trip() {
        let dir = std::env::temp_dir().join(format!("rekcod-syssock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("var")).unwrap();
        let sys = dir.join("var/docker.sock");
        let orb = dir.join("orbstack.sock");
        std::os::unix::fs::symlink(&orb, &sys).unwrap();
        // SAFETY: only this test reads REKCOD_SYSTEM_SOCKET.
        unsafe { std::env::set_var("REKCOD_SYSTEM_SOCKET", &sys) };
        let paths = Paths::at(dir.join("home")).unwrap();

        assert!(!on_start(&paths).unwrap(), "not installed: start leaves it alone");
        install(&paths, false).unwrap();
        assert!(installed(&paths));
        assert_eq!(std::fs::read_link(default_link(&paths)).unwrap(), orb);

        assert!(on_start(&paths).unwrap());
        assert_eq!(std::fs::read_link(default_link(&paths)).unwrap(), paths.socket());
        assert_eq!(state(&paths), "rekcod");
        on_stop(&paths).unwrap();
        assert_eq!(std::fs::read_link(default_link(&paths)).unwrap(), orb);

        uninstall(&paths).unwrap();
        assert_eq!(std::fs::read_link(&sys).unwrap(), orb);
        assert!(!default_link(&paths).exists() && std::fs::symlink_metadata(default_link(&paths)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
