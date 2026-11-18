//! RustFS, the fleet's S3 bucket (README.md, "Run S3-compatible storage").
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

/// 0600 from the start, written whole (temp file and rename); unchanged contents aren't
/// rewritten.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    crate::paths::write_private(path, data)
}

/// The keys in `file`, if it exists.
pub fn read_credentials(file: &Path) -> Result<S3> {
    let b = std::fs::read(file).with_context(|| format!("no storage credentials at {}", file.display()))?;
    serde_json::from_slice(&b).with_context(|| format!("{} isn't valid storage credentials", file.display()))
}

/// The root keys in `file`, or new ones when there is no such file. Any other failure is an
/// error: new keys would lock out every client that has the old ones.
fn root_keys(file: &Path) -> Result<S3> {
    match std::fs::metadata(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(S3 { endpoint: String::new(), access_key: format!("rekcod-{}", random_key(8)?), secret_key: random_key(40)?, region: None })
        }
        _ => read_credentials(file).context("fix or restore it; it holds the keys every client of this RustFS uses"),
    }
}

/// Where the pinned RustFS binary lives: `~/.rekcod/bin/rustfs-<version>`.
pub fn binary_path(paths: &Paths) -> PathBuf {
    paths.home.join("bin").join(format!("rustfs-{RUSTFS_VERSION}"))
}

/// [`binary_path`], downloading the pinned release first if needed.
pub fn ensure_binary(paths: &Paths) -> Result<PathBuf> {
    let bin = binary_path(paths);
    if bin.exists() {
        // Downloaded before rekcod patched it, on a Mac without Homebrew's xz.
        if !Path::new(BREW_LZMA).exists() {
            use_system_lzma(&bin)?;
        }
        return Ok(bin);
    }
    let parent = paths.home.join("bin");
    std::fs::create_dir_all(&parent)?;
    // `server setup` and a service start can both get here on a fresh Mac.
    // Not named `.rustfs-*`: `clear_stale` below removes those.
    let _lock = crate::paths::lock(&parent.join(".lock-rustfs-download"))?;
    if bin.exists() {
        return Ok(bin);
    }
    crate::fetch::clear_stale(&parent, ".rustfs-");
    let tmp = parent.join(format!(".rustfs-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let r = (|| -> Result<()> {
        eprintln!("rekcod: downloading RustFS {RUSTFS_VERSION}");
        let zip = tmp.join("rustfs.zip");
        if !crate::fetch::download_cmd(RUSTFS_ZIP, &zip, true).arg("-sS").status()?.success() {
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
        use_system_lzma(&tmp.join("rustfs"))?;
        std::fs::rename(tmp.join("rustfs"), &bin)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    r.map(|()| bin)
}

/// RustFS's macOS release loads liblzma from Homebrew's xz, so it fails to start on a Mac
/// without `brew install xz`. macOS ships the same library (compatibility version 14).
const BREW_LZMA: &str = "/opt/homebrew/opt/xz/lib/liblzma.5.dylib";
const SYSTEM_LZMA: &str = "/usr/lib/liblzma.5.dylib";

/// The load command's path, swapped in place: the new one is shorter, so the rest of the
/// field is zeroed and nothing moves. `None` when the path isn't there exactly once.
fn swap_lzma(bin: &[u8]) -> Option<Vec<u8>> {
    let needle = BREW_LZMA.as_bytes();
    let mut hits = bin.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).map(|(i, _)| i);
    let at = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    let mut out = bin.to_vec();
    out[at..at + needle.len()].fill(0);
    out[at..at + SYSTEM_LZMA.len()].copy_from_slice(SYSTEM_LZMA.as_bytes());
    Some(out)
}

/// Points `bin` at the system liblzma and signs it again (ad hoc: editing it broke the
/// signature, and arm64 won't run an unsigned binary). `/usr/bin/codesign` is on every Mac;
/// `install_name_tool` needs the Xcode tools.
fn use_system_lzma(bin: &Path) -> Result<()> {
    let bytes = std::fs::read(bin)?;
    let Some(patched) = swap_lzma(&bytes) else { return Ok(()) };
    let tmp = bin.with_extension("lzma-tmp");
    std::fs::write(&tmp, patched)?;
    std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    if !Command::new("/usr/bin/codesign").args(["-f", "-s", "-"]).arg(&tmp).stderr(Stdio::null()).status()?.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("could not sign {} after pointing it at {SYSTEM_LZMA}", bin.display());
    }
    std::fs::rename(&tmp, bin)?;
    eprintln!("rekcod: RustFS now loads {SYSTEM_LZMA} instead of Homebrew's");
    Ok(())
}

/// Runs RustFS in the foreground on `listen`, data in `data`, until it exits; exits with
/// its status. RustFS is a child rather than an exec so this process lives on to trim
/// storage.log, which launchd keeps appending RustFS's output to.
pub fn serve(paths: &Paths, listen: &str, data: Option<&Path>) -> Result<()> {
    crate::lifecycle::trim_log_hourly(paths.home.join("logs/storage.log"));
    let addr: SocketAddr = listen.parse().context("--listen takes IP:PORT")?;
    if addr.ip().is_unspecified() {
        bail!("--listen {listen}: give the address clients use (the Tailscale IP), not 0.0.0.0");
    }
    // App keys and presigned URLs cross this link; RustFS here speaks plain HTTP.
    crate::fleet::check_plain_http(listen, "storage")?;
    let data = data.map(Path::to_path_buf).unwrap_or_else(|| dir(paths).join("data"));
    std::fs::create_dir_all(&data)?;
    let file = credentials_file(paths);
    let mut creds = root_keys(&file)?;
    creds.endpoint = format!("http://{addr}");
    write_private(&file, &serde_json::to_vec_pretty(&creds)?)?;
    let bin = ensure_binary(paths)?;
    eprintln!("rekcod: RustFS {RUSTFS_VERSION} on {} (data {}, keys {})", creds.endpoint, data.display(), file.display());
    // Signals for this process go to RustFS; launchd stopping the job (or Ctrl-C) reaches
    // both anyway, since they share a process group.
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    let child = rustfs_command(&bin, listen, &data, &creds).spawn().context("start rustfs")?;
    let st = supervise(child, &SIGNAL)?;
    std::process::exit(exit_code(st));
}

static SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNAL.store(sig, std::sync::atomic::Ordering::SeqCst);
}

/// Waits for `child`, passing on each signal `pending` reports.
fn supervise(mut child: std::process::Child, pending: &std::sync::atomic::AtomicI32) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(st) = child.try_wait()? {
            return Ok(st);
        }
        let sig = pending.swap(0, std::sync::atomic::Ordering::SeqCst);
        if sig != 0 {
            unsafe { libc::kill(child.id() as libc::pid_t, sig) };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The exit code a shell would give for `st`: its code, or 128 + the signal.
fn exit_code(st: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}

/// RustFS on `listen` only. Its console is off: RustFS 1.0.1 otherwise also serves the
/// console, the S3 API and the admin API on `*:9001`, every interface.
fn rustfs_command(bin: &Path, listen: &str, data: &Path, creds: &S3) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(["server", "--address", listen])
        .arg(data)
        .env("RUSTFS_CONSOLE_ENABLE", "false")
        .env("RUSTFS_ACCESS_KEY", &creds.access_key)
        .env("RUSTFS_SECRET_KEY", &creds.secret_key);
    cmd
}

// ------------------------------------------------------------------ local dev

fn docker(paths: &Paths, args: &[&str]) -> Result<String> {
    docker_env(paths, args, &[])
}

/// `docker args` with `env` in the CLI's environment: `-e NAME` takes the value from there,
/// so secrets stay out of the process list.
fn docker_env(paths: &Paths, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let mut cmd = crate::docker::any_cli(paths)?;
    let out = cmd.args(["-H", &paths.docker_host()]).args(args).envs(env.iter().copied()).stdin(Stdio::null()).output()?;
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
        let mut creds = root_keys(&local_file(paths))?;
        // A stopped one keeps its port and data; a port taken since then means a new one.
        let _ = docker(paths, &["rm", "-f", LOCAL_CONTAINER]);
        let old = creds.endpoint.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        let port = match old.filter(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok()) {
            Some(p) => p,
            None => free_port_from(9000)?,
        };
        creds.endpoint = format!("http://127.0.0.1:{port}");
        docker_env(
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
                "RUSTFS_ACCESS_KEY",
                "-e",
                "RUSTFS_SECRET_KEY",
                "-e",
                "RUSTFS_VOLUMES=/data",
                "-e",
                "RUSTFS_ADDRESS=:9000",
                LOCAL_IMAGE,
            ],
            &[("RUSTFS_ACCESS_KEY", &creds.access_key), ("RUSTFS_SECRET_KEY", &creds.secret_key)],
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

/// Where `snap --push/--pull` go: `$S3_ENDPOINT`/`$S3_BUCKET` with the `AWS_*` keys (and
/// `AWS_REGION`) if set, else the local RustFS and bucket `rekcod-snaps`. A plain `http://`
/// endpoint must be on loopback or the tailnet (or `REKCOD_INSECURE=1`): volume archives
/// would cross the network unencrypted.
pub fn snap_target(paths: &Paths) -> Result<(S3, String)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(t) = env_target(&var)? {
        return Ok(t);
    }
    let s = local(paths)?;
    s.create_bucket("rekcod-snaps")?;
    Ok((s, "rekcod-snaps".into()))
}

fn env_target(var: &dyn Fn(&str) -> Option<String>) -> Result<Option<(S3, String)>> {
    let (Some(endpoint), Some(access_key), Some(secret_key)) = (var("S3_ENDPOINT"), var("AWS_ACCESS_KEY_ID"), var("AWS_SECRET_ACCESS_KEY"))
    else {
        return Ok(None);
    };
    let bucket = var("S3_BUCKET").context("S3_ENDPOINT is set but S3_BUCKET isn't")?;
    let endpoint = endpoint.trim_end_matches('/').to_string();
    match (endpoint.strip_prefix("http://"), endpoint.starts_with("https://")) {
        (Some(authority), _) => crate::fleet::check_plain_http(authority, "S3_ENDPOINT")?,
        (None, true) => {}
        _ => bail!("S3_ENDPOINT={endpoint}: use an https:// or http:// URL"),
    }
    Ok(Some((S3 { endpoint, access_key, secret_key, region: var("AWS_REGION") }, bucket)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signal for `storage serve` reaches RustFS, and RustFS's exit status is serve's.
    #[test]
    fn rustfs_runs_as_a_child_and_gets_the_signals() {
        let pending = std::sync::atomic::AtomicI32::new(0);
        let child = Command::new("/bin/sh").args(["-c", "trap 'exit 7' TERM; while :; do sleep 0.05; done"]).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        pending.store(libc::SIGTERM, std::sync::atomic::Ordering::SeqCst);
        let st = supervise(child, &pending).unwrap();
        assert_eq!(exit_code(st), 7);
        let killed = Command::new("/bin/sh").args(["-c", "kill -KILL $$"]).status().unwrap();
        assert_eq!(exit_code(killed), 128 + libc::SIGKILL);
    }

    #[test]
    fn rustfs_serves_only_its_listen_address() {
        let creds = S3 { endpoint: String::new(), access_key: "ak".into(), secret_key: "sk".into(), region: None };
        let cmd = rustfs_command(Path::new("/bin/rustfs"), "127.0.0.1:7710", Path::new("/data"), &creds);
        let envs: Vec<_> =
            cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        assert!(envs.contains(&("RUSTFS_CONSOLE_ENABLE".into(), Some("false".into()))), "{envs:?}");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["server", "--address", "127.0.0.1:7710", "/data"]);
        assert!(!args.iter().any(|a| a.contains("sk")), "the secret goes in the environment");
    }

    #[test]
    fn unreadable_credentials_are_an_error_not_new_keys() {
        let dir = std::env::temp_dir().join(format!("rekcod-storage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("credentials.json");
        let made = root_keys(&file).unwrap();
        assert!(made.access_key.starts_with("rekcod-") && made.secret_key.len() == 40);
        write_private(&file, &serde_json::to_vec(&made).unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(root_keys(&file).unwrap().secret_key, made.secret_key);
        std::fs::write(&file, b"").unwrap();
        assert!(root_keys(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_stores_over_https_or_a_private_link() {
        let env =
            |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        let base: &[(&str, &str)] = &[("AWS_ACCESS_KEY_ID", "a"), ("AWS_SECRET_ACCESS_KEY", "s"), ("S3_BUCKET", "b")];
        assert!(env_target(&env(base)).unwrap().is_none());
        let https = [base, &[("S3_ENDPOINT", "https://s3.eu-west-1.amazonaws.com/"), ("AWS_REGION", "eu-west-1")]].concat().leak();
        let (s3, bucket) = env_target(&env(https)).unwrap().unwrap();
        assert_eq!(
            (s3.endpoint.as_str(), s3.region.as_deref(), bucket.as_str()),
            ("https://s3.eu-west-1.amazonaws.com", Some("eu-west-1"), "b")
        );
        let tailnet = [base, &[("S3_ENDPOINT", "http://100.100.1.2:9000")]].concat().leak();
        assert!(env_target(&env(tailnet)).unwrap().is_some());
        if std::env::var_os("REKCOD_INSECURE").is_none() {
            let lan = [base, &[("S3_ENDPOINT", "http://192.168.1.10:9000")]].concat().leak();
            assert!(env_target(&env(lan)).is_err());
        }
        let ftp = [base, &[("S3_ENDPOINT", "ftp://x")]].concat().leak();
        assert!(env_target(&env(ftp)).is_err());
    }

    #[test]
    fn lzma_path_swapped_in_place() {
        let bin = [b"\x0c\0\0\0head".as_slice(), BREW_LZMA.as_bytes(), b"\0\0tail"].concat();
        let out = swap_lzma(&bin).unwrap();
        assert_eq!(out.len(), bin.len());
        assert_eq!(&out[8..8 + SYSTEM_LZMA.len()], SYSTEM_LZMA.as_bytes());
        assert!(out[8 + SYSTEM_LZMA.len()..8 + BREW_LZMA.len()].iter().all(|&b| b == 0));
        assert!(out.ends_with(b"\0\0tail"));
        // Already patched, or the path twice: leave it alone.
        assert_eq!(swap_lzma(&out), None);
        assert_eq!(swap_lzma(&[bin.clone(), bin].concat()), None);
    }
}
