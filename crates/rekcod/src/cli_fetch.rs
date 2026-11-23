//! Bundled docker CLI, compose and buildx for machines without a docker CLI
//! (`rekcod <docker command>`). Downloaded on first use, sha256-pinned, using the
//! system `curl` and `tar`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const CLI_VERSION: &str = "29.10.0";

struct Asset {
    url: &'static str,
    sha256: &'static str,
    /// Install path under the version dir.
    dest: &'static str,
    /// Member to extract when `url` is a .tgz.
    member: Option<&'static str>,
}

const ASSETS: &[Asset] = &[
    Asset {
        url: "https://download.docker.com/mac/static/stable/aarch64/docker-29.10.0.tgz",
        sha256: "84025e01f7716eb8bd1064c49a9d2b3a38547fa963983e81017c73ee8c5d540b",
        dest: "docker",
        member: Some("docker/docker"),
    },
    Asset {
        url: "https://github.com/docker/compose/releases/download/v5.8.0/docker-compose-darwin-aarch64",
        sha256: "008b8fcf6c6d60340f5796626a0f0d51c042085793a9b1653343011727140b71",
        dest: "cli-plugins/docker-compose",
        member: None,
    },
    Asset {
        url: "https://github.com/docker/buildx/releases/download/v0.39.1/buildx-v0.39.1.darwin-arm64",
        sha256: "7a7f10602f0826d36b046e8db012b4cbfaa4ba56f9d2a5d374fe5bb7ae864fc4",
        dest: "cli-plugins/docker-buildx",
        member: None,
    },
];

/// `~/.rekcod/cli/<version>`, with `docker` and `cli-plugins/`, downloading it first if needed.
pub fn ensure_cli(rekcod_home: &Path) -> Result<PathBuf> {
    let dir = rekcod_home.join("cli").join(CLI_VERSION);
    if ASSETS.iter().all(|a| dir.join(a.dest).exists()) {
        return Ok(dir);
    }
    let parent = rekcod_home.join("cli");
    std::fs::create_dir_all(&parent)?;
    // Another command may be downloading (or already running docker from `dir`): wait for
    // it instead of deleting the directory under it.
    let _lock = crate::paths::lock(&parent.join(".download.lock"))?;
    if ASSETS.iter().all(|a| dir.join(a.dest).exists()) {
        return Ok(dir);
    }
    crate::fetch::clear_stale(&parent, ".download-");
    let tmp = parent.join(format!(".download-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("unpacked/cli-plugins"))?;
    eprintln!("rekcod: no docker CLI found; downloading docker {CLI_VERSION} with compose and buildx");
    let result = ASSETS.iter().try_for_each(|a| download(a, &tmp)).and_then(|()| {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(tmp.join("unpacked"), &dir).with_context(|| format!("install {}", dir.display()))
    });
    let _ = std::fs::remove_dir_all(&tmp);
    result.map(|()| dir)
}

fn download(a: &Asset, tmp: &Path) -> Result<()> {
    let file = tmp.join(a.url.rsplit('/').next().unwrap_or("download"));
    let ok = crate::fetch::download_cmd(a.url, &file, true).arg("-sS").status().is_ok_and(|s| s.success());
    if !ok {
        bail!("could not download {}", a.url);
    }
    let out = Command::new("shasum").args(["-a", "256"]).arg(&file).output().context("run shasum")?;
    let got = crate::fetch::parse_shasum(&String::from_utf8_lossy(&out.stdout)).map(str::to_string).unwrap_or_default();
    if !got.eq_ignore_ascii_case(a.sha256) {
        bail!("checksum mismatch for {}: expected {}, got {got:?}", a.url, a.sha256);
    }
    let dest = tmp.join("unpacked").join(a.dest);
    match a.member {
        Some(member) => {
            let ok = Command::new("tar").arg("-xzf").arg(&file).arg("-C").arg(tmp).arg(member).status()?.success();
            if !ok {
                bail!("could not unpack {}", a.url);
            }
            std::fs::rename(tmp.join(member), &dest)?;
        }
        None => std::fs::rename(&file, &dest)?,
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}
