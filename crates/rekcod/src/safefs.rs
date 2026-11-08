//! Reads and writes inside a directory without following symlinks, for `sandbox apply`.
//!
//! Every path component is opened with `O_NOFOLLOW` relative to the base directory's fd.
//! A symlink anywhere on the way (one the sandbox wrote in an earlier apply, one already
//! in the project, or one swapped into the copy being read) makes the call fail instead of
//! reaching outside the base. Entries passed to [`Dir::protect`] are recognised by device
//! and inode, so a name the filesystem resolves to the same entry (`.GIT` on
//! case-insensitive APFS, another Unicode normalization of the name) can't reach them.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub struct Dir {
    fd: OwnedFd,
    /// (device, inode, name) of entries nothing may go into, replace or remove.
    protected: Vec<(libc::dev_t, libc::ino_t, OsString)>,
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

/// Whether a symlink at `rel` pointing at `target` stays inside the base directory, read
/// lexically. Other links in the tree can still take it out; `sandbox.rs` resolves chains.
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

/// An entry's kind (`f`, `l`, `d` or `o`), size, mtime in ns and permission bits, without
/// following a final symlink. Manifests written before the mode was recorded have `None`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct State(pub char, pub u64, pub i128, #[serde(default)] pub Option<u32>);

impl State {
    pub fn kind(&self) -> char {
        self.0
    }
}

fn kind_of(mode: libc::mode_t) -> char {
    match mode & libc::S_IFMT {
        libc::S_IFREG => 'f',
        libc::S_IFLNK => 'l',
        libc::S_IFDIR => 'd',
        _ => 'o',
    }
}

fn stat_at(fd: RawFd, name: &OsStr) -> Option<libc::stat> {
    let c = cstr(name).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    (unsafe { libc::fstatat(fd, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } == 0).then_some(st)
}

impl Dir {
    pub fn open(path: &Path) -> Result<Dir> {
        let c = cstr(path.as_os_str())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(last_err("open", path.as_os_str()));
        }
        Ok(Dir { fd: unsafe { OwnedFd::from_raw_fd(fd) }, protected: vec![] })
    }

    /// Refuses, from now on, any path that reaches the entry `name` (if it exists) under
    /// whatever spelling the filesystem accepts for it.
    pub fn protect(&mut self, name: &str) {
        if let Some(st) = stat_at(self.fd.as_raw_fd(), OsStr::new(name)) {
            self.protected.push((st.st_dev, st.st_ino, name.into()));
        }
    }

    fn guard(&self, st: &libc::stat, rel: &Path) -> Result<()> {
        if let Some((_, _, name)) = self.protected.iter().find(|(d, i, _)| *d == st.st_dev && *i == st.st_ino) {
            bail!("refusing {}: it's the project's {}", rel.display(), name.to_string_lossy());
        }
        Ok(())
    }

    /// Refuses `name` in `dir` if it is a protected entry.
    fn guard_entry(&self, dir: &Dir, name: &OsStr, rel: &Path) -> Result<()> {
        match stat_at(dir.fd.as_raw_fd(), name) {
            Some(st) => self.guard(&st, rel),
            None => Ok(()),
        }
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
        Ok(Dir { fd: unsafe { OwnedFd::from_raw_fd(fd) }, protected: vec![] })
    }

    /// The directory `rel` names, every component opened without following links.
    fn open_dir(&self, rel: &Path, create: bool) -> Result<Dir> {
        let mut d = Dir { fd: self.fd.try_clone()?, protected: vec![] };
        for name in names(rel)? {
            d = d.sub(name, create).with_context(|| format!("in {}", rel.display()))?;
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(d.fd.as_raw_fd(), &mut st) } != 0 {
                return Err(last_err("stat", rel.as_os_str()));
            }
            self.guard(&st, rel)?;
        }
        Ok(d)
    }

    /// The directory holding `rel`'s last component, and that component's name.
    fn parent(&self, rel: &Path, create: bool) -> Result<(Dir, OsString)> {
        let n = names(rel)?;
        let (last, dirs) = n.split_last().context("empty path")?;
        let d = match dirs.is_empty() {
            true => Dir { fd: self.fd.try_clone()?, protected: vec![] },
            false => self.open_dir(&dirs.iter().collect::<PathBuf>(), create).with_context(|| format!("in {}", rel.display()))?,
        };
        Ok((d, last.to_os_string()))
    }

    fn unlink(&self, name: &OsStr, flags: libc::c_int) -> Result<()> {
        let c = cstr(name)?;
        if unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), flags) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENOENT) {
                return Err(anyhow!("remove {}: {e}", name.to_string_lossy()));
            }
        }
        Ok(())
    }

    /// Opens the regular file `rel` for reading. A symlink at `rel` or on the way to it,
    /// or anything but a regular file there, is an error.
    pub fn open_file(&self, rel: &Path) -> Result<std::fs::File> {
        let (dir, name) = self.parent(rel, false)?;
        let c = cstr(&name)?;
        // O_NONBLOCK: a FIFO put there opens at once and is refused below, instead of hanging.
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(dir.fd.as_raw_fd(), c.as_ptr(), flags) };
        if fd < 0 {
            return Err(last_err("open", rel.as_os_str()));
        }
        let f = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        if !f.metadata()?.is_file() {
            bail!("{} isn't a regular file", rel.display());
        }
        Ok(f)
    }

    /// The target of the symlink `rel`, reached without following any other link.
    pub fn read_link(&self, rel: &Path) -> Result<PathBuf> {
        let (dir, name) = self.parent(rel, false)?;
        let c = cstr(&name)?;
        let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
        let n = unsafe { libc::readlinkat(dir.fd.as_raw_fd(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(last_err("readlink", rel.as_os_str()));
        }
        buf.truncate(n as usize);
        Ok(PathBuf::from(OsString::from_vec(buf)))
    }

    /// Replaces `rel` with a copy of `input`, a regular file. Permission bits are kept,
    /// setuid, setgid and sticky are not.
    pub fn write_file(&self, rel: &Path, mut input: std::fs::File) -> Result<()> {
        let meta = input.metadata()?;
        if !meta.is_file() {
            bail!("{} isn't a regular file", rel.display());
        }
        let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777;
        let (dir, name) = self.parent(rel, true)?;
        self.guard_entry(&dir, &name, rel)?;
        dir.unlink(&name, 0)?;
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
        self.guard_entry(&dir, &name, rel)?;
        dir.unlink(&name, 0)?;
        let (t, n) = (cstr(target.as_os_str())?, cstr(&name)?);
        if unsafe { libc::symlinkat(t.as_ptr(), dir.fd.as_raw_fd(), n.as_ptr()) } != 0 {
            return Err(last_err("symlink", rel.as_os_str()));
        }
        Ok(())
    }

    /// Removes the file or symlink at `rel` (not a directory).
    pub fn remove(&self, rel: &Path) -> Result<()> {
        let (dir, name) = self.parent(rel, false)?;
        self.guard_entry(&dir, &name, rel)?;
        dir.unlink(&name, 0)
    }

    /// Removes the empty directory at `rel`.
    pub fn remove_dir(&self, rel: &Path) -> Result<()> {
        let (dir, name) = self.parent(rel, false)?;
        self.guard_entry(&dir, &name, rel)?;
        dir.unlink(&name, libc::AT_REMOVEDIR)
    }

    /// Everything under the directory `rel`, as (path from the base, kind), parents before
    /// their children. Symlinks are listed, not followed.
    pub fn tree(&self, rel: &Path) -> Result<Vec<(PathBuf, char)>> {
        let mut out = Vec::new();
        self.open_dir(rel, false)?.list_into(rel, &mut out)?;
        Ok(out)
    }

    fn list_into(&self, rel: &Path, out: &mut Vec<(PathBuf, char)>) -> Result<()> {
        for name in self.entry_names()? {
            let Some(st) = stat_at(self.fd.as_raw_fd(), &name) else { continue };
            let (kind, path) = (kind_of(st.st_mode), rel.join(&name));
            out.push((path.clone(), kind));
            if kind == 'd' {
                self.sub(&name, false)?.list_into(&path, out)?;
            }
        }
        Ok(())
    }

    fn entry_names(&self) -> Result<Vec<OsString>> {
        let fd = unsafe { libc::dup(self.fd.as_raw_fd()) };
        if fd < 0 {
            bail!("dup: {}", std::io::Error::last_os_error());
        }
        let d = unsafe { libc::fdopendir(fd) };
        if d.is_null() {
            unsafe { libc::close(fd) };
            bail!("fdopendir: {}", std::io::Error::last_os_error());
        }
        // The dup shares the offset of earlier reads of this fd.
        unsafe { libc::rewinddir(d) };
        let mut out = Vec::new();
        loop {
            let e = unsafe { libc::readdir(d) };
            if e.is_null() {
                break;
            }
            let name = unsafe { CStr::from_ptr((*e).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                out.push(OsStr::from_bytes(name).to_os_string());
            }
        }
        unsafe { libc::closedir(d) };
        Ok(out)
    }

    /// `rel`'s state, or None if it doesn't exist or can't be reached without a symlink.
    pub fn state(&self, rel: &Path) -> Option<State> {
        let (dir, name) = self.parent(rel, false).ok()?;
        let st = stat_at(dir.fd.as_raw_fd(), &name)?;
        let mtime = st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128;
        Some(State(kind_of(st.st_mode), st.st_size as u64, mtime, Some(st.st_mode as u32 & 0o7777)))
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

    fn file(p: &Path) -> std::fs::File {
        std::fs::File::open(p).unwrap()
    }

    #[test]
    fn writes_never_follow_symlinks() {
        let (base, outside, src) = (tmp("base"), tmp("outside"), tmp("src"));
        std::fs::write(src.join("payload"), "x").unwrap();
        std::fs::write(outside.join("victim"), "safe").unwrap();
        // A symlinked file at the destination is replaced, not written through.
        std::os::unix::fs::symlink(outside.join("victim"), base.join("rc")).unwrap();
        let d = Dir::open(&base).unwrap();
        d.write_file(Path::new("rc"), file(&src.join("payload"))).unwrap();
        assert_eq!(std::fs::read_to_string(outside.join("victim")).unwrap(), "safe");
        assert_eq!(std::fs::read_to_string(base.join("rc")).unwrap(), "x");
        // A symlinked directory on the way makes the write fail.
        std::os::unix::fs::symlink(&outside, base.join("dir")).unwrap();
        assert!(d.write_file(Path::new("dir/new"), file(&src.join("payload"))).is_err());
        assert!(!outside.join("new").exists());
        assert!(d.write_file(Path::new("../escape"), file(&src.join("payload"))).is_err());
        assert!(d.state(Path::new("dir/victim")).is_none());
        assert_eq!(d.state(Path::new("rc")).unwrap().kind(), 'f');
    }

    #[test]
    fn reads_never_follow_symlinks() {
        let (src, outside) = (tmp("read-src"), tmp("read-outside"));
        std::fs::write(outside.join("secret"), "host only").unwrap();
        std::fs::create_dir(src.join("real")).unwrap();
        std::fs::write(src.join("real/f"), "ok").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("dir")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), src.join("link")).unwrap();
        let d = Dir::open(&src).unwrap();
        let mut s = String::new();
        d.open_file(Path::new("real/f")).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "ok");
        // A symlinked parent and a symlinked file are both refused, so a swap in the copy
        // being applied can't pull Mac files in.
        assert!(d.open_file(Path::new("dir/secret")).is_err());
        assert!(d.open_file(Path::new("link")).is_err());
        assert!(d.read_link(Path::new("dir/x")).is_err());
        assert_eq!(d.read_link(Path::new("link")).unwrap(), outside.join("secret"));
        let fifo = CString::new(src.join("pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        assert!(d.open_file(Path::new("pipe")).is_err());
        let mut t = d.tree(Path::new("real")).unwrap();
        t.sort();
        assert_eq!(t, vec![(PathBuf::from("real/f"), 'f')]);
        assert!(d.tree(Path::new("dir")).is_err());
    }

    #[test]
    fn protected_entries_are_matched_by_identity() {
        let (base, src) = (tmp("protect"), tmp("protect-src"));
        std::fs::write(src.join("payload"), "x").unwrap();
        std::fs::create_dir(base.join(".git")).unwrap();
        // NFC on disk; written below as NFD.
        std::fs::create_dir(base.join("caf\u{e9}")).unwrap();
        let mut d = Dir::open(&base).unwrap();
        d.protect(".git");
        d.protect("caf\u{e9}");
        // On a case- or normalization-sensitive filesystem an alias is a separate directory
        // and writing there is fine; where it resolves to the protected one it must fail.
        let ci = base.join(".GIT").exists();
        let ni = base.join("cafe\u{301}").exists();
        for (alias, aliased) in [(".git/x", true), (".GIT/x", ci), (".Git/x", ci), ("cafe\u{301}/x", ni), ("caf\u{e9}/x", true)] {
            let r = d.write_file(Path::new(alias), file(&src.join("payload")));
            assert!(!aliased || r.is_err(), "{alias} was written");
        }
        assert_eq!(std::fs::read_dir(base.join(".git")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(base.join("caf\u{e9}")).unwrap().count(), 0);
        assert!(d.remove_dir(Path::new(".GIT")).is_err() || !base.join(".GIT").exists());
        assert!(base.join(".git").is_dir());
        // A `.git` file (a worktree's) can't be replaced either.
        let wt = tmp("protect-wt");
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere").unwrap();
        let mut d = Dir::open(&wt).unwrap();
        d.protect(".git");
        assert!(d.write_file(Path::new(".git"), file(&src.join("payload"))).is_err());
        assert!(d.write_symlink(Path::new(".git"), Path::new("x")).is_err());
        assert!(d.remove(Path::new(".git")).is_err());
        assert_eq!(std::fs::read_to_string(wt.join(".git")).unwrap(), "gitdir: /elsewhere");
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

    #[test]
    fn state_records_mode_and_reads_old_manifests() {
        let base = tmp("mode");
        std::fs::write(base.join("f"), "x").unwrap();
        std::fs::set_permissions(base.join("f"), std::os::unix::fs::PermissionsExt::from_mode(0o640)).unwrap();
        let d = Dir::open(&base).unwrap();
        let st = d.state(Path::new("f")).unwrap();
        assert_eq!((st.kind(), st.1, st.3), ('f', 1, Some(0o640)));
        let old: std::collections::BTreeMap<String, State> = serde_json::from_str(r#"{"f": ["f", 1, 5]}"#).unwrap();
        assert_eq!(old["f"], State('f', 1, 5, None));
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back, st);
    }
}
