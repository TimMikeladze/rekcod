//! RustFS, the fleet's S3 bucket (docs/rustfs.md).
//!
//! - `rekcod storage serve`: the pinned RustFS release binary, run on this Mac with root keys
//!   from `storage/credentials.json` (created on first run).
//! - `rekcod storage up|down|env`: RustFS in the local VM for development.

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::s3::S3;

pub const RUSTFS_VERSION: &str = "1.0.1";
const RUSTFS_ZIP: &str = "https://github.com/rustfs/rustfs/releases/download/1.0.1/rustfs-macos-aarch64-v1.0.1.zip";
const RUSTFS_SHA256: &str = "18aac7101c3484b98f93de64a0609aaa8e02aba072ba7927172c4b2eae7d4843";
const LOCAL_IMAGE: &str = "rustfs/rustfs:1.0.1";
const LOCAL_CONTAINER: &str = "rekcod-storage";

fn dir(paths: &Paths) -> PathBuf {
    paths.home.join("storage")
}

/// Root keys of the RustFS this Mac serves (`storage serve`).
pub fn credentials_file(paths: &Paths) -> PathBuf {
    dir(paths).join("credentials.json")
}

fn random_key(n: usize) -> Result<String> {
    let mut s = String::new();
    while s.len() < n {
        s.push_str(&crate::sandbox::new_id()?);
    }
    s.truncate(n);
    Ok(s)
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    std::fs::write(path, data)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// The keys in `file`, if it exists.
pub fn read_credentials(file: &Path) -> Result<S3> {
    let b = std::fs::read(file).with_context(|| format!("no storage credentials at {}", file.display()))?;
    Ok(serde_json::from_slice(&b)?)
}

/// `~/.rekcod/bin/rustfs-<version>`, downloading the pinned release first if needed.
pub fn ensure_binary(paths: &Paths) -> Result<PathBuf> {
    let bin = paths.home.join("bin").join(format!("rustfs-{RUSTFS_VERSION}"));
    if bin.exists() {
        return Ok(bin);
    }
    let tmp = paths.home.join("bin").join(format!(".rustfs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let r = (|| -> Result<()> {
        eprintln!("rekcod: downloading RustFS {RUSTFS_VERSION}");
        let zip = tmp.join("rustfs.zip");
        if !Command::new("curl").args(["-fsSL", "-o"]).arg(&zip).arg(RUSTFS_ZIP).status()?.success() {
            bail!("could not download {RUSTFS_ZIP}");
        }
        let out = Command::new("shasum").args(["-a", "256"]).arg(&zip).output()?;
        let got = crate::fetch::parse_shasum(&String::from_utf8_lossy(&out.stdout)).unwrap_or_default().to_string();
        if !got.eq_ignore_ascii_case(RUSTFS_SHA256) {
            bail!("checksum mismatch for {RUSTFS_ZIP}: expected {RUSTFS_SHA256}, got {got:?}");
        }
        if !Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&zip).arg(&tmp).status()?.success() {
            bail!("could not unpack {}", zip.display());
        }
        // Unsigned: a quarantine flag would stop launchd from running it.
        let _ = Command::new("/usr/bin/xattr").args(["-d", "com.apple.quarantine"]).arg(tmp.join("rustfs")).stderr(Stdio::null()).status();
        std::fs::rename(tmp.join("rustfs"), &bin)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    r.map(|()| bin)
}

/// Runs RustFS in the foreground (it replaces this process) on `listen`, data in `data`.
pub fn serve(paths: &Paths, listen: &str, data: Option<&Path>) -> Result<()> {
    crate::lifecycle::trim_log_hourly(paths.home.join("logs/storage.log"));
    let addr: SocketAddr = listen.parse().context("--listen takes IP:PORT")?;
    if addr.ip().is_unspecified() {
        bail!("--listen {listen}: give the address clients use (the Tailscale IP), not 0.0.0.0");
    }
    let data = data.map(Path::to_path_buf).unwrap_or_else(|| dir(paths).join("data"));
    std::fs::create_dir_all(&data)?;
    let file = credentials_file(paths);
    let mut creds = match read_credentials(&file) {
        Ok(c) => c,
        Err(_) => S3 { endpoint: String::new(), access_key: format!("rekcod-{}", random_key(8)?), secret_key: random_key(40)? },
    };
    creds.endpoint = format!("http://{addr}");
    write_private(&file, &serde_json::to_vec_pretty(&creds)?)?;
    let bin = ensure_binary(paths)?;
    eprintln!("rekcod: RustFS {RUSTFS_VERSION} on {} (data {}, keys {})", creds.endpoint, data.display(), file.display());
    use std::os::unix::process::CommandExt;
    let err = Command::new(bin)
        .args(["server", "--address", listen])
        .arg(&data)
        .env("RUSTFS_ACCESS_KEY", &creds.access_key)
        .env("RUSTFS_SECRET_KEY", &creds.secret_key)
        .exec();
    Err(err).context("exec rustfs")
}

// ------------------------------------------------------------------ local dev

fn docker(paths: &Paths, args: &[&str]) -> Result<String> {
    let mut cmd = crate::docker::any_cli(paths)?;
    let out = cmd.args(["-H", &paths.docker_host()]).args(args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        bail!("docker {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn local_file(paths: &Paths) -> PathBuf {
    dir(paths).join("local.json")
}

/// The first port from `from` that nothing on this Mac listens on.
pub fn free_port_from(from: u16) -> Result<u16> {
    (from..from.saturating_add(100)).find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok()).context("no free port")
}

pub fn env_block(s: &S3, bucket: Option<&str>) -> String {
    let mut out = format!(
        "S3_ENDPOINT={}\nAWS_ACCESS_KEY_ID={}\nAWS_SECRET_ACCESS_KEY={}\nAWS_REGION={}\n",
        s.endpoint,
        s.access_key,
        s.secret_key,
        crate::s3::REGION
    );
    if let Some(b) = bucket {
        out.push_str(&format!("S3_BUCKET={b}\n"));
    }
    out
}

/// The local RustFS's keys, if `storage up` has run.
pub fn local(paths: &Paths) -> Result<S3> {
    read_credentials(&local_file(paths)).context("no local storage; `rekcod storage up`")
}

pub fn local_running(paths: &Paths) -> bool {
    docker(paths, &["inspect", "-f", "{{.State.Running}}", LOCAL_CONTAINER]).is_ok_and(|s| s == "true")
}

/// Starts `rekcod-storage` in the VM (data in volume `rekcod-storage`) on a free port and
/// prints the env block. Running it again prints the block for the running one.
pub fn up(paths: &Paths) -> Result<()> {
    if !local_running(paths) {
        let mut creds = read_credentials(&local_file(paths)).unwrap_or(S3 {
            endpoint: String::new(),
            access_key: format!("rekcod-{}", random_key(8)?),
            secret_key: random_key(40)?,
        });
        // A stopped one keeps its port and data; a port taken since then means a new one.
        let _ = docker(paths, &["rm", "-f", LOCAL_CONTAINER]);
        let old = creds.endpoint.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        let port = match old.filter(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok()) {
            Some(p) => p,
            None => free_port_from(9000)?,
        };
        creds.endpoint = format!("http://127.0.0.1:{port}");
        docker(
            paths,
            &[
                "run",
                "-d",
                "--name",
                LOCAL_CONTAINER,
                "--restart",
                "unless-stopped",
                "-p",
                &format!("127.0.0.1:{port}:9000"),
                "-v",
                "rekcod-storage:/data",
                "-e",
                &format!("RUSTFS_ACCESS_KEY={}", creds.access_key),
                "-e",
                &format!("RUSTFS_SECRET_KEY={}", creds.secret_key),
                "-e",
                "RUSTFS_VOLUMES=/data",
                "-e",
                "RUSTFS_ADDRESS=:9000",
                LOCAL_IMAGE,
            ],
        )?;
        write_private(&local_file(paths), &serde_json::to_vec_pretty(&creds)?)?;
        let t0 = std::time::Instant::now();
        while creds.request("GET", "/", &[], crate::s3::Body::Empty).map(|r| r.status).unwrap_or(0) != 200 {
            if t0.elapsed().as_secs() > 30 {
                bail!("RustFS didn't answer on {} within 30s; `rekcod logs {LOCAL_CONTAINER}`", creds.endpoint);
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        eprintln!("rekcod: RustFS {RUSTFS_VERSION} on {} (data in volume rekcod-storage)", creds.endpoint);
    }
    print!("{}", env_block(&local(paths)?, None));
    Ok(())
}

pub fn down(paths: &Paths) -> Result<()> {
    docker(paths, &["rm", "-f", LOCAL_CONTAINER])?;
    println!("stopped {LOCAL_CONTAINER} (data kept in volume rekcod-storage)");
    Ok(())
}

pub fn env(paths: &Paths) -> Result<()> {
    print!("{}", env_block(&local(paths)?, None));
    Ok(())
}

/// Where `snap --push/--pull` go: `$S3_ENDPOINT`/`$S3_BUCKET` with the `AWS_*` keys if set,
/// else the local RustFS and bucket `rekcod-snaps`.
pub fn snap_target(paths: &Paths) -> Result<(S3, String)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let (Some(endpoint), Some(access_key), Some(secret_key)) =
        (var("S3_ENDPOINT"), var("AWS_ACCESS_KEY_ID"), var("AWS_SECRET_ACCESS_KEY"))
    {
        let bucket = var("S3_BUCKET").context("S3_ENDPOINT is set but S3_BUCKET isn't")?;
        return Ok((S3 { endpoint, access_key, secret_key }, bucket));
    }
    let s = local(paths)?;
    s.create_bucket("rekcod-snaps")?;
    Ok((s, "rekcod-snaps".into()))
}
