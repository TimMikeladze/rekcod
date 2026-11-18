//! rekcod's own SSH key for hosts that only take a password.
//!
//! `~/.rekcod/ssh/id_ed25519` and `~/.rekcod/ssh/config` (one block per host) are rekcod's;
//! the user's `~/.ssh/config` only gets an `Include` of the latter, because docker's ssh
//! reads nothing else.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;

const INCLUDE_START: &str = "# >>> rekcod: keys for rekcod hosts (https://github.com/TimMikeladze/rekcod#what-host-add-changes)";
/// What 0.3.0 wrote, still recognised when the Include comes out.
const INCLUDE_START_0_3: &str = "# >>> rekcod: keys for rekcod hosts (docs/ssh-keys.md)";
const INCLUDE_END: &str = "# <<< rekcod";

pub fn key(paths: &Paths) -> PathBuf {
    paths.home.join("ssh/id_ed25519")
}

fn config(paths: &Paths) -> PathBuf {
    paths.home.join("ssh/config")
}

fn user_config() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join(".ssh/config"))
}

/// The host in `user@host[:port]`, as ssh matches it against `Host`.
pub fn host_of(dest: &str) -> &str {
    let rest = dest.split_once('@').map_or(dest, |(_, r)| r);
    match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => h,
        _ => rest,
    }
}

fn start(name: &str) -> String {
    format!("# >>> rekcod host {name}")
}

fn end(name: &str) -> String {
    format!("# <<< rekcod host {name}")
}

/// `cfg` without the lines from a `start` to the next `end`, both included. A `start` with
/// no `end` after it is an error and nothing is removed: everything below it would go.
/// Bytes, so a config with non-UTF-8 in it comes back as it was.
fn without(cfg: &[u8], start: &[&str], end: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(cfg.len());
    let mut skipping = None;
    for line in cfg.split_inclusive(|b| *b == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        if skipping.is_none() && start.iter().any(|s| text == s.as_bytes()) {
            skipping = Some(String::from_utf8_lossy(text).into_owned());
        } else if skipping.is_some() && text == end.as_bytes() {
            skipping = None;
        } else if skipping.is_none() {
            out.extend_from_slice(text);
            out.push(b'\n');
        }
    }
    if let Some(s) = skipping {
        bail!("`{s}` has no `{end}` after it; fix that by hand (rekcod changes nothing past an unterminated block)");
    }
    Ok(out)
}

fn without_str(cfg: &str, start: &str, end: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&without(cfg.as_bytes(), &[start], end)?).into_owned())
}

/// `cfg` with host NAME's block replaced by one for `host` using `key`.
pub fn set_block(cfg: &str, name: &str, host: &str, key: &Path) -> Result<String> {
    let mut out = without_str(cfg, &start(name), &end(name))?;
    out.push_str(&format!("{}\nHost {host}\n  IdentityFile \"{}\"\n  IdentitiesOnly yes\n{}\n", start(name), key.display(), end(name)));
    Ok(out)
}

pub fn remove_block(cfg: &str, name: &str) -> Result<String> {
    without_str(cfg, &start(name), &end(name))
}

/// The `Host` in NAME's block.
pub fn block_host(cfg: &str, name: &str) -> Option<String> {
    let s = start(name);
    let mut lines = cfg.lines().skip_while(|l| *l != s);
    lines.next()?;
    lines.next()?.strip_prefix("Host ").map(String::from)
}

/// The user's ssh config with the `Include` of rekcod's at the top, once. Include goes
/// first: after a `Host` line it would only apply to that host.
pub fn with_include(user: &[u8], rekcod: &Path) -> Vec<u8> {
    let line = format!("Include \"{}\"", rekcod.display());
    if user.split(|b| *b == b'\n').any(|l| l == line.as_bytes()) {
        return user.to_vec();
    }
    let mut out = format!("{INCLUDE_START}\n{line}\n{INCLUDE_END}\n\n").into_bytes();
    out.extend_from_slice(user);
    out
}

pub fn without_include(user: &[u8]) -> Result<Vec<u8>> {
    let out = without(user, &[INCLUDE_START, INCLUDE_START_0_3], INCLUDE_END)?;
    Ok(out.strip_prefix(b"\n").map(<[u8]>::to_vec).unwrap_or(out))
}

/// `path`'s bytes, or none when it doesn't exist. Any other error is returned: treating an
/// unreadable config as empty would write over it.
fn read_or_empty(path: &Path) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Replaces `path` (0600) with `text`. A symlinked config (dotfiles) is written through,
/// so the link stays.
fn write_private(path: &Path, text: &[u8]) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().context("no parent")?;
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("rekcod-tmp");
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// `ssh-keygen` for rekcod's key, unless it exists.
fn ensure_key(paths: &Paths) -> Result<PathBuf> {
    let k = key(paths);
    if k.exists() {
        return Ok(k);
    }
    std::fs::create_dir_all(k.parent().context("no parent")?)?;
    let me = Command::new("scutil").args(["--get", "LocalHostName"]).output().ok().filter(|o| o.status.success());
    let comment = format!("rekcod@{}", me.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "mac".into()));
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
        .arg(&k)
        .stdin(Stdio::null())
        .status()
        .context("run ssh-keygen")?
        .success();
    if !ok {
        bail!("ssh-keygen failed to create {}", k.display());
    }
    eprintln!("rekcod: created {} ({comment})", k.display());
    Ok(k)
}

/// Makes `dest` take rekcod's key: `ssh-copy-id` (the password prompt) and a config block,
/// so plain `ssh dest` and docker's ssh use it. Needs a terminal for the prompt.
pub fn install(paths: &Paths, name: &str, dest: &str) -> Result<()> {
    let k = key(paths);
    // Already in its authorized_keys (copied by hand, or the config was lost): only the
    // config is missing, and no password is needed.
    if k.exists() && accepts(dest, &k) {
        return write_config(paths, name, host_of(dest), &k);
    }
    if !std::io::stdin().is_terminal() {
        let k = ensure_key(paths)?;
        bail!(
            "{dest} wants a password, and there's no terminal to type it in; run this in one, or run\n  ssh-copy-id -i {} {dest}\nand then this again",
            k.with_extension("pub").display()
        );
    }
    let k = ensure_key(paths)?;
    let host = host_of(dest);
    eprintln!("rekcod: {dest} needs rekcod's key once; ssh-copy-id asks for the password on that Mac");
    let mut copy = Command::new("ssh-copy-id");
    copy.arg("-i").arg(k.with_extension("pub")).args(["-o", "StrictHostKeyChecking=accept-new"]);
    if let Some((_, p)) = dest.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()) {
        copy.args(["-p", p]);
    }
    let target = dest.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()).map_or(dest, |(d, _)| d);
    if !copy.arg("--").arg(target).status().context("run ssh-copy-id")?.success() {
        bail!("ssh-copy-id to {dest} failed");
    }
    write_config(paths, name, host, &k)
}

/// Whether `dest` lets rekcod's key in, without asking for anything.
fn accepts(dest: &str, k: &Path) -> bool {
    let mut ssh = Command::new("ssh");
    ssh.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "IdentitiesOnly=yes", "-i"]).arg(k);
    match dest.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()) {
        Some((d, p)) => ssh.args(["-p", p, "--", d]),
        None => ssh.args(["--", dest]),
    };
    ssh.arg("true").stdin(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Host NAME's block in rekcod's config, and the Include in the user's.
fn write_config(paths: &Paths, name: &str, host: &str, k: &Path) -> Result<()> {
    let cfg = String::from_utf8_lossy(&read_or_empty(&config(paths))?).into_owned();
    write_private(&config(paths), set_block(&cfg, name, host, k)?.as_bytes())?;
    include_in(&user_config()?, &config(paths))
}

/// The Include of `rekcod` at the top of the user's config `user`. The first time rekcod
/// changes that file, what was there is kept as `config.rekcod-backup`.
fn include_in(user: &Path, rekcod: &Path) -> Result<()> {
    let current = read_or_empty(user)?;
    let next = with_include(&current, rekcod);
    if next == current {
        return Ok(());
    }
    let backup = user.with_extension("rekcod-backup");
    if !current.is_empty() && !backup.exists() {
        write_private(&backup, &current)?;
    }
    write_private(user, &next)?;
    eprintln!("rekcod: {} now includes {}", user.display(), rekcod.display());
    Ok(())
}

/// Host NAME's block follows it to `host` (a LAN server found under a new name).
pub fn retarget(paths: &Paths, name: &str, host: &str) -> Result<()> {
    let cfg = String::from_utf8_lossy(&read_or_empty(&config(paths))?).into_owned();
    if block_host(&cfg, name).is_none() {
        return Ok(());
    }
    write_private(&config(paths), set_block(&cfg, name, host, &key(paths))?.as_bytes())
}

pub fn forget(paths: &Paths, name: &str) -> Result<()> {
    let cfg = String::from_utf8_lossy(&read_or_empty(&config(paths))?).into_owned();
    let next = remove_block(&cfg, name)?;
    if next != cfg {
        write_private(&config(paths), next.as_bytes())?;
    }
    Ok(())
}

/// `rekcod uninstall`: the `Include` out of the user's config.
pub fn remove_include() -> Result<()> {
    remove_include_from(&user_config()?)
}

fn remove_include_from(user: &Path) -> Result<()> {
    let cfg = read_or_empty(user)?;
    let next = without_include(&cfg)?;
    if next != cfg {
        write_private(user, &next)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_host() {
        assert_eq!(host_of("tim@tailbus.local"), "tailbus.local");
        assert_eq!(host_of("tim@tailbus.local:2222"), "tailbus.local");
        assert_eq!(host_of("mini"), "mini");
    }

    /// Blocks are replaced, moved and removed by name; everything else stays as it was.
    #[test]
    fn blocks() {
        let k = Path::new("/Users/me/.rekcod/ssh/id_ed25519");
        let cfg = set_block("Host other\n  User x\n", "tailbus", "tailbus.local", k).unwrap();
        assert!(cfg.starts_with("Host other\n  User x\n# >>> rekcod host tailbus\nHost tailbus.local\n"));
        assert!(cfg.contains("IdentityFile \"/Users/me/.rekcod/ssh/id_ed25519\"\n  IdentitiesOnly yes\n"));
        assert_eq!(block_host(&cfg, "tailbus").as_deref(), Some("tailbus.local"));
        let moved = set_block(&cfg, "tailbus", "Tailbus-2.local", k).unwrap();
        assert_eq!(moved.matches(">>> rekcod host tailbus").count(), 1);
        assert_eq!(block_host(&moved, "tailbus").as_deref(), Some("Tailbus-2.local"));
        let two = set_block(&moved, "mini", "mini.local", k).unwrap();
        assert_eq!(remove_block(&two, "mini").unwrap(), moved);
        assert_eq!(remove_block(&moved, "tailbus").unwrap(), "Host other\n  User x\n");
        // A start without its end: nothing below it is dropped, nothing is added.
        let cut = "# >>> rekcod host tailbus\nHost tailbus.local\nHost mine\n  User me\n";
        assert!(remove_block(cut, "tailbus").is_err());
        assert!(set_block(cut, "tailbus", "x.local", k).is_err());
    }

    #[test]
    fn include_goes_first_once() {
        let p = Path::new("/Users/me/.rekcod/ssh/config");
        let user = b"Include ~/.orbstack/ssh/config\n\nHost *\n  AddKeysToAgent yes\n";
        let with = with_include(user, p);
        assert!(with.starts_with(format!("{INCLUDE_START}\nInclude \"/Users/me/.rekcod/ssh/config\"\n{INCLUDE_END}\n\n").as_bytes()));
        assert_eq!(with_include(&with, p), with);
        assert_eq!(without_include(&with).unwrap(), user);
        assert_eq!(with_include(b"", p).split(|b| *b == b'\n').count(), 5);
        // 0.3.0's marker still comes out.
        let old = format!("{INCLUDE_START_0_3}\nInclude \"x\"\n{INCLUDE_END}\n\nHost a\n");
        assert_eq!(without_include(old.as_bytes()).unwrap(), b"Host a\n");
    }

    /// A config that isn't UTF-8 keeps every byte under the Include, the original is
    /// backed up once, and the Include comes out again byte for byte.
    #[test]
    fn non_utf8_config_survives() {
        let dir = std::env::temp_dir().join(format!("rekcod-sshcfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (user, rekcod) = (dir.join("config"), dir.join("rk/config"));
        let original = b"# caf\xe9 bastion\nHost b\n  User me\n".to_vec();
        std::fs::write(&user, &original).unwrap();
        include_in(&user, &rekcod).unwrap();
        let now = std::fs::read(&user).unwrap();
        assert!(now.starts_with(INCLUDE_START.as_bytes()) && now.ends_with(&original));
        assert_eq!(std::fs::read(user.with_extension("rekcod-backup")).unwrap(), original);
        remove_include_from(&user).unwrap();
        assert_eq!(std::fs::read(&user).unwrap(), original);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
