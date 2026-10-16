//! Writes into a project directory without following symlinks, for `sandbox apply`.
//!
//! Every path component is opened with `O_NOFOLLOW` relative to the project's directory fd.
//! A symlink anywhere on the way (one the sandbox wrote in an earlier apply, or one already
//! in the project) makes the write fail instead of landing outside the project.

use std::ffi::{CString, OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use anyhow::{Context, Result, anyhow, bail};

pub struct Dir {
    fd: OwnedFd,
}

fn cstr(s: &OsStr) -> Result<CString> {
    Ok(CString::new(s.as_bytes())?)
}

fn last_err(what: &str, name: &OsStr) -> anyhow::Error {
    anyhow!("{what} {}: {}", name.to_string_lossy(), std::io::Error::last_os_error())
}

/// The plain names in `rel`; refuses `..`, `/` and anything else that could leave the base.
fn names(rel: &Path) -> Result<Vec<&OsStr>> {
    let mut out = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            _ => bail!("refusing path {}", rel.display()),
        }
    }
    if out.is_empty() {
        bail!("empty path");
    }
    Ok(out)
}

/// Whether a symlink at `rel` pointing at `target` stays inside the base directory.
pub fn link_stays_inside(rel: &Path, target: &Path) -> bool {
    let mut depth = rel.parent().map_or(0, |p| p.components().filter(|c| matches!(c, Component::Normal(_))).count()) as i64;
    for c in target.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// (kind, size, mtime in ns) of an entry, without following a final symlink.
pub type State = (char, u64, i128);

impl Dir {
    pub fn open(path: &Path) -> Result<Dir> {
        let c = cstr(path.as_os_str())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(last_err("open", path.as_os_str()));
        }
        Ok(Dir { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    fn sub(&self, name: &OsStr, create: bool) -> Result<Dir> {
        let c = cstr(name)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(self.fd.as_raw_fd(), c.as_ptr(), flags) };
        if fd < 0 && create && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            if unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), 0o755) } != 0 {
                return Err(last_err("mkdir", name));
            }
            fd = unsafe { libc::openat(self.fd.as_raw_fd(), c.as_ptr(), flags) };
        }
        if fd < 0 {
            // ELOOP/ENOTDIR here means a symlink or a file where a directory should be.
            return Err(last_err("open directory", name));
        }
        Ok(Dir { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// The directory holding `rel`'s last component, and that component's name.
    fn parent(&self, rel: &Path, create: bool) -> Result<(Dir, OsString)> {
        let n = names(rel)?;
        let (last, dirs) = n.split_last().context("empty path")?;
        let mut d = Dir { fd: self.fd.try_clone()? };
        for name in dirs {
            d = d.sub(name, create).with_context(|| format!("in {}", rel.display()))?;
        }
        Ok((d, last.to_os_string()))
    }

    fn unlink(&self, name: &OsStr) -> Result<()> {
        let c = cstr(name)?;
        if unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENOENT) {
                return Err(anyhow!("remove {}: {e}", name.to_string_lossy()));
            }
        }
        Ok(())
    }

    /// Replaces `rel` with a copy of the regular file `src`. Permission bits are kept,
    /// setuid, setgid and sticky are not.
    pub fn write_file(&self, rel: &Path, src: &Path) -> Result<()> {
        let mut input =
            std::fs::OpenOptions::new().read(true).custom_flags_nofollow().open(src).with_context(|| format!("open {}", src.display()))?;
        let meta = input.metadata()?;
        if !meta.is_file() {
            bail!("{} isn't a regular file", src.display());
        }
        let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777;
        let (dir, name) = self.parent(rel, true)?;
        dir.unlink(&name)?;
        let c = cstr(&name)?;
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(dir.fd.as_raw_fd(), c.as_ptr(), flags, mode as libc::c_uint) };
        if fd < 0 {
            return Err(last_err("create", rel.as_os_str()));
        }
        let mut out = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
        // The umask may have narrowed the create mode.
        if unsafe { libc::fchmod(out.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(last_err("chmod", rel.as_os_str()));
        }
        Ok(())
    }

    /// Replaces `rel` with a symlink to `target`, which must stay inside the base directory.
    pub fn write_symlink(&self, rel: &Path, target: &Path) -> Result<()> {
        if !link_stays_inside(rel, target) {
            bail!("refusing symlink {} -> {}: it points outside the project", rel.display(), target.display());
        }
        let (dir, name) = self.parent(rel, true)?;
        dir.unlink(&name)?;
        let (t, n) = (cstr(target.as_os_str())?, cstr(&name)?);
        if unsafe { libc::symlinkat(t.as_ptr(), dir.fd.as_raw_fd(), n.as_ptr()) } != 0 {
            return Err(last_err("symlink", rel.as_os_str()));
        }
        Ok(())
    }

    /// Removes the file or symlink at `rel` (not a directory).
    pub fn remove(&self, rel: &Path) -> Result<()> {
        let (dir, name) = self.parent(rel, false)?;
        dir.unlink(&name)
    }

    /// `rel`'s state, or None if it doesn't exist or can't be reached without a symlink.
    pub fn state(&self, rel: &Path) -> Option<State> {
        let (dir, name) = self.parent(rel, false).ok()?;
        let c = cstr(&name).ok()?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(dir.fd.as_raw_fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return None;
        }
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFREG => 'f',
            libc::S_IFLNK => 'l',
            libc::S_IFDIR => 'd',
            _ => 'o',
        };
        Some((kind, st.st_size as u64, st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128))
    }
}

trait NoFollow {
    fn custom_flags_nofollow(&mut self) -> &mut Self;
}

impl NoFollow for std::fs::OpenOptions {
    fn custom_flags_nofollow(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::custom_flags(self, libc::O_NOFOLLOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rekcod-safefs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn writes_never_follow_symlinks() {
        let (base, outside, src) = (tmp("base"), tmp("outside"), tmp("src"));
        std::fs::write(src.join("payload"), "x").unwrap();
        std::fs::write(outside.join("victim"), "safe").unwrap();
        // A symlinked file at the destination is replaced, not written through.
        std::os::unix::fs::symlink(outside.join("victim"), base.join("rc")).unwrap();
        let d = Dir::open(&base).unwrap();
        d.write_file(Path::new("rc"), &src.join("payload")).unwrap();
        assert_eq!(std::fs::read_to_string(outside.join("victim")).unwrap(), "safe");
        assert_eq!(std::fs::read_to_string(base.join("rc")).unwrap(), "x");
        // A symlinked directory on the way makes the write fail.
        std::os::unix::fs::symlink(&outside, base.join("dir")).unwrap();
        assert!(d.write_file(Path::new("dir/new"), &src.join("payload")).is_err());
        assert!(!outside.join("new").exists());
        // A symlinked source is refused, so a swap in the snapshot can't pull Mac files in.
        std::os::unix::fs::symlink(outside.join("victim"), src.join("link")).unwrap();
        assert!(d.write_file(Path::new("f"), &src.join("link")).is_err());
        assert!(d.write_file(Path::new("../escape"), &src.join("payload")).is_err());
        assert!(d.state(Path::new("dir/victim")).is_none());
        assert_eq!(d.state(Path::new("rc")).unwrap().0, 'f');
    }

    #[test]
    fn symlinks_must_stay_inside() {
        assert!(link_stays_inside(Path::new("a/b"), Path::new("../c")));
        assert!(link_stays_inside(Path::new("a"), Path::new("sub/x")));
        assert!(!link_stays_inside(Path::new("a"), Path::new("../x")));
        assert!(!link_stays_inside(Path::new("a/b"), Path::new("../../x")));
        assert!(!link_stays_inside(Path::new("a"), Path::new("/etc/passwd")));
        let base = tmp("links");
        let d = Dir::open(&base).unwrap();
        assert!(d.write_symlink(Path::new("rc"), Path::new("/Users/me/.zshrc")).is_err());
        d.write_symlink(Path::new("ok"), Path::new("rc")).unwrap();
        assert_eq!(std::fs::read_link(base.join("ok")).unwrap(), Path::new("rc"));
    }
}
