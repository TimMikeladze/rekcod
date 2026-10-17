//! rekcod's own SSH key for hosts that only take a password (docs/ssh-keys.md).
//!
//! `~/.rekcod/ssh/id_ed25519` and `~/.rekcod/ssh/config` (one block per host) are rekcod's;
//! the user's `~/.ssh/config` only gets an `Include` of the latter, because docker's ssh
//! reads nothing else.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;

const INCLUDE_START: &str = "# >>> rekcod: keys for rekcod hosts (docs/ssh-keys.md)";
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

/// `cfg` without the lines from `start` to `end`, both included.
fn without(cfg: &str, start: &str, end: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in cfg.lines() {
        if line == start {
            skipping = true;
        } else if skipping && line == end {
            skipping = false;
        } else if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `cfg` with host NAME's block replaced by one for `host` using `key`.
pub fn set_block(cfg: &str, name: &str, host: &str, key: &Path) -> String {
    let mut out = without(cfg, &start(name), &end(name));
    out.push_str(&format!("{}\nHost {host}\n  IdentityFile \"{}\"\n  IdentitiesOnly yes\n{}\n", start(name), key.display(), end(name)));
    out
}

pub fn remove_block(cfg: &str, name: &str) -> String {
    without(cfg, &start(name), &end(name))
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
pub fn with_include(user: &str, rekcod: &Path) -> String {
    let line = format!("Include \"{}\"", rekcod.display());
    if user.lines().any(|l| l == line) {
        return user.to_string();
    }
    format!("{INCLUDE_START}\n{line}\n{INCLUDE_END}\n\n{user}")
}

pub fn without_include(user: &str) -> String {
    let out = without(user, INCLUDE_START, INCLUDE_END);
    out.strip_prefix('\n').map(String::from).unwrap_or(out)
}

/// Replaces `path` (0600) with `text`. A symlinked config (dotfiles) is written through,
/// so the link stays.
fn write_private(path: &Path, text: &str) -> Result<()> {
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
    if !std::io::stdin().is_terminal() {
        bail!("{dest} wants a password, and there's no terminal to type it in; run this in one, or `ssh-copy-id {dest}`");
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
    if !copy.arg(target).status().context("run ssh-copy-id")?.success() {
        bail!("ssh-copy-id to {dest} failed");
    }
    let cfg = std::fs::read_to_string(config(paths)).unwrap_or_default();
    write_private(&config(paths), &set_block(&cfg, name, host, &k))?;
    let user = user_config()?;
    let current = std::fs::read_to_string(&user).unwrap_or_default();
    let next = with_include(&current, &config(paths));
    if next != current {
        write_private(&user, &next)?;
        eprintln!("rekcod: {} now includes {}", user.display(), config(paths).display());
    }
    Ok(())
}

/// Host NAME's block follows it to `host` (a LAN server found under a new name).
pub fn retarget(paths: &Paths, name: &str, host: &str) -> Result<()> {
    let cfg = std::fs::read_to_string(config(paths)).unwrap_or_default();
    if block_host(&cfg, name).is_none() {
        return Ok(());
    }
    write_private(&config(paths), &set_block(&cfg, name, host, &key(paths)))
}

pub fn forget(paths: &Paths, name: &str) -> Result<()> {
    let Ok(cfg) = std::fs::read_to_string(config(paths)) else { return Ok(()) };
    let next = remove_block(&cfg, name);
    if next != cfg {
        write_private(&config(paths), &next)?;
    }
    Ok(())
}

/// `rekcod uninstall`: the `Include` out of the user's config.
pub fn remove_include() -> Result<()> {
    let user = user_config()?;
    let Ok(cfg) = std::fs::read_to_string(&user) else { return Ok(()) };
    let next = without_include(&cfg);
    if next != cfg {
        write_private(&user, &next)?;
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
        let cfg = set_block("Host other\n  User x\n", "tailbus", "tailbus.local", k);
        assert!(cfg.starts_with("Host other\n  User x\n# >>> rekcod host tailbus\nHost tailbus.local\n"));
        assert!(cfg.contains("IdentityFile \"/Users/me/.rekcod/ssh/id_ed25519\"\n  IdentitiesOnly yes\n"));
        assert_eq!(block_host(&cfg, "tailbus").as_deref(), Some("tailbus.local"));
        let moved = set_block(&cfg, "tailbus", "Tailbus-2.local", k);
        assert_eq!(moved.matches(">>> rekcod host tailbus").count(), 1);
        assert_eq!(block_host(&moved, "tailbus").as_deref(), Some("Tailbus-2.local"));
        let two = set_block(&moved, "mini", "mini.local", k);
        assert_eq!(remove_block(&two, "mini"), moved);
        assert_eq!(remove_block(&moved, "tailbus"), "Host other\n  User x\n");
    }

    #[test]
    fn include_goes_first_once() {
        let p = Path::new("/Users/me/.rekcod/ssh/config");
        let user = "Include ~/.orbstack/ssh/config\n\nHost *\n  AddKeysToAgent yes\n";
        let with = with_include(user, p);
        assert!(with.starts_with(&format!("{INCLUDE_START}\nInclude \"/Users/me/.rekcod/ssh/config\"\n{INCLUDE_END}\n\n")));
        assert_eq!(with_include(&with, p), with);
        assert_eq!(without_include(&with), user);
        assert_eq!(with_include("", p).lines().count(), 4);
    }
}
