//! Downloads the guest a release binary was built with (docs/phase3.md).
//!
//! Release builds embed `REKCOD_GUEST_VERSION` and `REKCOD_GUEST_SHA256`; dev
//! builds embed neither and use `out/guest`. Uses the system `curl`, `shasum`
//! and `tar`, all part of macOS.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const GUEST_VERSION: Option<&str> = option_env!("REKCOD_GUEST_VERSION");
pub const GUEST_SHA256: Option<&str> = option_env!("REKCOD_GUEST_SHA256");
const REPO: &str = "TimMikeladze/rekcod";

pub fn tarball_name(guest_version: &str) -> String {
    format!("rekcod-guest-{guest_version}.tar.gz")
}

/// Release download URL for `file`; `$REKCOD_RELEASE_URL` overrides the base.
pub fn release_url(base: Option<&str>, version: &str, file: &str) -> String {
    match base {
        Some(b) => format!("{}/{file}", b.trim_end_matches('/')),
        None => format!("https://github.com/{REPO}/releases/download/v{version}/{file}"),
    }
}

/// First field of `shasum -a 256` output.
pub fn parse_shasum(out: &str) -> Option<&str> {
    let hash = out.split_whitespace().next()?;
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

/// `~/.rekcod/guest/<version>`, downloading and verifying it first if needed.
/// `None` for builds without an embedded guest (dev builds).
pub fn ensure_guest(rekcod_home: &Path) -> Result<Option<PathBuf>> {
    let (Some(gv), Some(sha)) = (GUEST_VERSION, GUEST_SHA256) else { return Ok(None) };
    let dir = rekcod_home.join("guest").join(gv);
    if dir.join("Image").exists() && dir.join("root.img").exists() {
        return Ok(Some(dir));
    }
    let parent = dir.parent().unwrap_or(rekcod_home);
    std::fs::create_dir_all(parent)?;
    // Two commands starting at once on a fresh install: one downloads, the other waits.
    let _lock = crate::paths::lock(&parent.join(".download.lock"))?;
    if dir.join("Image").exists() && dir.join("root.img").exists() {
        return Ok(Some(dir));
    }
    let tmp = parent.join(format!(".download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let result = download(gv, sha, &tmp)
        .and_then(|()| std::fs::rename(tmp.join("unpacked"), &dir).with_context(|| format!("install {}", dir.display())));
    let _ = std::fs::remove_dir_all(&tmp);
    result?;
    prune_old_guests(parent, gv);
    Ok(Some(dir))
}

/// Deletes guests other than `keep` under `~/.rekcod/guest/`: the root disk is a copy and
/// the kernel is read at boot, so nothing uses an old one after an upgrade.
fn prune_old_guests(parent: &Path, keep: &str) {
    for e in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name != keep && !name.starts_with('.') && e.path().join("root.img").exists() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn download(gv: &str, sha: &str, tmp: &Path) -> Result<()> {
    let file = tarball_name(gv);
    let tarball = tmp.join(&file);
    let version = env!("CARGO_PKG_VERSION");
    let base = std::env::var("REKCOD_RELEASE_URL").ok();
    let url = release_url(base.as_deref(), version, &file);
    eprintln!("rekcod: downloading the guest image ({file})");
    let curl = Command::new("curl").args(["-fL", "--progress-bar", "-o"]).arg(&tarball).arg(&url).status();
    if !curl.is_ok_and(|s| s.success()) {
        // Fallback for networks where curl to GitHub fails but an authenticated gh works.
        let gh = Command::new("gh").args(["release", "download", &format!("v{version}"), "-R", REPO, "-p", &file, "-D"]).arg(tmp).status();
        if !gh.is_ok_and(|s| s.success()) {
            bail!("could not download {url} (check your connection, or set REKCOD_RELEASE_URL)");
        }
    }
    let out = Command::new("shasum").args(["-a", "256"]).arg(&tarball).output().context("run shasum")?;
    let got = parse_shasum(&String::from_utf8_lossy(&out.stdout)).map(str::to_string).unwrap_or_default();
    if !got.eq_ignore_ascii_case(sha) {
        bail!("guest image checksum mismatch: expected {sha}, got {got:?}; not installing it");
    }
    let unpacked = tmp.join("unpacked");
    std::fs::create_dir_all(&unpacked)?;
    let tar = Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&unpacked).status()?;
    if !tar.success() || !unpacked.join("root.img").exists() {
        bail!("could not unpack {file}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            release_url(None, "0.2.0", "rekcod-guest-ab12.tar.gz"),
            "https://github.com/TimMikeladze/rekcod/releases/download/v0.2.0/rekcod-guest-ab12.tar.gz"
        );
        assert_eq!(release_url(Some("http://localhost:8000/"), "0.2.0", "x.tar.gz"), "http://localhost:8000/x.tar.gz");
    }

    #[test]
    fn shasum_output() {
        let h = "a".repeat(64);
        assert_eq!(parse_shasum(&format!("{h}  /tmp/x.tar.gz\n")), Some(h.as_str()));
        assert_eq!(parse_shasum("shasum: /tmp/x: No such file"), None);
        assert_eq!(parse_shasum(""), None);
    }
}
