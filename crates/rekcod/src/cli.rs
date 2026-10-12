//! User-facing commands: start, stop, status, import, shell, and docker passthrough.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::docker::{self, CONTEXT};
use crate::http;
use crate::paths::Paths;
use crate::vm;

const HELPER_IMAGE: &str = "alpine:3.22";

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime").block_on(f)
}

/// GET on the rekcod docker socket.
fn api_get(paths: &Paths, path: &str) -> Result<Vec<u8>> {
    block_on(async {
        let stream = tokio::net::UnixStream::connect(paths.socket()).await?;
        tokio::time::timeout(Duration::from_secs(5), http::get(stream, path)).await?
    })
}

fn running_pid(paths: &Paths) -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(paths.pid_file()).ok()?.trim().parse().ok()?;
    (unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
}

pub(crate) fn sysctl_u64(name: &str) -> Option<u64> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&mut value as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0)
    };
    (rc == 0).then_some(value)
}

fn random_mac() -> Result<String> {
    let mut b = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[0] = (b[0] & 0xfe) | 0x02; // unicast, locally administered
    Ok(b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":"))
}

/// Root disk follows the installed guest version; data disk and MAC persist.
fn prepare_vm(paths: &Paths, disk_gib: u64) -> Result<()> {
    let guest = paths.guest_dir()?;
    let version = std::fs::read_to_string(guest.join("VERSION")).unwrap_or_else(|_| "dev".into());
    let installed = std::fs::read_to_string(paths.guest_version()).unwrap_or_default();
    if !paths.root_disk().exists() || installed.trim() != version.trim() {
        // std::fs::copy clones on APFS, so this is instant and takes no space.
        std::fs::copy(guest.join("root.img"), paths.root_disk()).context("install root disk")?;
        std::fs::write(paths.guest_version(), &version)?;
    }
    // Sparse; the guest grows the filesystem to match at boot.
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(paths.data_disk())?;
    if f.metadata()?.len() < disk_gib << 30 {
        f.set_len(disk_gib << 30)?;
    }
    if !paths.mac_address().exists() {
        std::fs::write(paths.mac_address(), random_mac()?)?;
    }
    Ok(())
}

fn tail(path: &std::path::Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Boots the VM unless it is running; returns the pid when it already was.
fn boot(paths: &Paths, cpus: Option<usize>, memory_gib: Option<u64>, disk_gib: u64) -> Result<Option<i32>> {
    if let Some(pid) = running_pid(paths) {
        return Ok(Some(pid));
    }
    {
        prepare_vm(paths, disk_gib)?;
        let host_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let cpus = cpus.unwrap_or(host_cpus).clamp(1, vm::max_cpus());
        let host_mib = sysctl_u64("hw.memsize").unwrap_or(16 << 30) >> 20;
        let memory_mib = memory_gib.map(|g| g << 10).unwrap_or((host_mib / 2).min(8 << 10));

        let log = OpenOptions::new().create(true).append(true).open(paths.daemon_log())?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args(["daemon", "--cpus", &cpus.to_string(), "--memory-mib", &memory_mib.to_string()])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // Detach from the terminal so closing it doesn't kill the VM.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn().context("spawn rekcod daemon")?;

        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if api_get(paths, "/_ping").is_ok_and(|b| b == b"OK") {
                break;
            }
            if let Some(status) = child.try_wait()? {
                bail!(
                    "daemon exited ({status}).\n--- {} ---\n{}\n--- {} ---\n{}",
                    paths.daemon_log().display(),
                    tail(&paths.daemon_log(), 15),
                    paths.console_log().display(),
                    tail(&paths.console_log(), 25),
                );
            }
            if Instant::now() > deadline {
                bail!("dockerd did not answer within 120s; see {}", paths.console_log().display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(None)
}

pub fn start(paths: &Paths, cpus: Option<usize>, memory_gib: Option<u64>, disk_gib: u64, no_switch: bool) -> Result<()> {
    let t0 = Instant::now();
    if let Some(pid) = boot(paths, cpus, memory_gib, disk_gib)? {
        println!("rekcod is already running (pid {pid})");
    }
    let version = api_get(paths, "/version")
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["Version"].as_str().map(String::from))
        .unwrap_or_else(|| "?".into());
    println!("rekcod ready in {:.1}s · dockerd {version}", t0.elapsed().as_secs_f64());

    if !docker::available() {
        println!("no docker CLI installed: use `rekcod <docker command>` (e.g. `rekcod ps`, `rekcod compose up`),");
        println!("or point other clients at it: export DOCKER_HOST={}", paths.docker_host());
        return Ok(());
    }
    docker::ensure_context(&paths.docker_host())?;
    if no_switch {
        println!("docker context `{CONTEXT}` is available (not switched: --no-switch)");
    } else {
        let previous = docker::current_context().unwrap_or_else(|_| "default".into());
        if previous != CONTEXT {
            std::fs::write(paths.prev_context(), &previous)?;
            docker::use_context(CONTEXT)?;
            println!("docker context: {CONTEXT} (was {previous}; `rekcod stop` switches back)");
        } else {
            println!("docker context: {CONTEXT}");
        }
    }
    if std::env::var_os("DOCKER_HOST").is_some() {
        println!("warning: DOCKER_HOST is set and overrides docker contexts; `unset DOCKER_HOST` to use rekcod");
    }
    Ok(())
}

pub fn stop(paths: &Paths, no_switch: bool) -> Result<()> {
    match running_pid(paths) {
        None => println!("rekcod is not running"),
        Some(pid) => {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(45);
            while unsafe { libc::kill(pid, 0) } == 0 {
                if Instant::now() > deadline {
                    bail!("daemon (pid {pid}) did not exit; see {}", paths.daemon_log().display());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            println!("rekcod stopped");
        }
    }
    if !no_switch && docker::available() && docker::current_context().is_ok_and(|c| c == CONTEXT) {
        let mut previous = std::fs::read_to_string(paths.prev_context()).unwrap_or_default().trim().to_string();
        if previous.is_empty() || !docker::context_exists(&previous) {
            previous = "default".into();
        }
        docker::use_context(&previous)?;
        println!("docker context: {previous}");
    }
    Ok(())
}

pub fn status(paths: &Paths) -> Result<()> {
    let Some(pid) = running_pid(paths) else {
        println!("rekcod is not running (`rekcod start`)");
        return Ok(());
    };
    let status: serde_json::Value = std::fs::read(paths.status_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let version: serde_json::Value = api_get(paths, "/version")
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    println!("rekcod running (pid {pid})");
    println!("  dockerd   {} (API {})", version["Version"].as_str().unwrap_or("?"), version["ApiVersion"].as_str().unwrap_or("?"));
    println!("  kernel    {}", version["KernelVersion"].as_str().unwrap_or("?"));
    println!("  vm        {} CPUs, {} MiB", status["cpus"], status["memory_mib"]);
    let sharing = if status["fs_cache"].as_bool() == Some(true) { "cached, invalidated by FSEvents" } else { "uncached (stock virtio-fs)" };
    println!("  files     {sharing}");
    if let (Some(target), Some(max)) = (status["balloon_mib"].as_u64(), status["memory_mib"].as_u64())
        && target < max
    {
        println!("  balloon   guest limited to {target} of {max} MiB (REKCOD_BALLOON, experimental)");
    }
    println!("  socket    {}", paths.docker_host());
    if docker::available() {
        println!("  context   {}", docker::current_context().unwrap_or_else(|_| "?".into()));
    }
    let ports = status["ports"].as_array().cloned().unwrap_or_default();
    if ports.is_empty() {
        println!("  ports     none published");
    }
    for p in ports {
        let addr = p["address"].as_str().unwrap_or("?");
        let addr = if addr == "::" { "*".to_string() } else { addr.to_string() };
        let note = p["error"].as_str().map(|e| format!("  ! not forwarded: {e}")).unwrap_or_default();
        let udp = if p["proto"].as_str() == Some("udp") { "/udp" } else { "" };
        println!("  port      {addr}:{}{udp} -> {}{note}", p["port"], p["container"].as_str().unwrap_or("?"));
    }
    println!("  logs      {} , {}", paths.daemon_log().display(), paths.console_log().display());
    Ok(())
}

pub fn compact(paths: &Paths) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let used = || std::fs::metadata(paths.data_disk()).map(|m| m.blocks() * 512).unwrap_or(0);
    let before = used();
    let reply = crate::daemon::send_control(paths, &rekcod_proto::Control::Trim)?;
    if !reply.starts_with("ok") {
        bail!("compact failed: {reply}");
    }
    let after = used();
    let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
    println!("data disk: {:.2} GiB -> {:.2} GiB on the Mac ({:.2} GiB returned)", gib(before), gib(after), gib(before.saturating_sub(after)));
    Ok(())
}

/// `docker <args>` against rekcod's socket, booting the VM first if needed.
pub fn passthrough(paths: &Paths, args: &[String]) -> Result<std::convert::Infallible> {
    if running_pid(paths).is_none() {
        let t0 = Instant::now();
        eprintln!("rekcod: starting the VM");
        boot(paths, None, None, crate::paths::DEFAULT_DISK_GIB)?;
        eprintln!("rekcod: ready in {:.1}s", t0.elapsed().as_secs_f64());
    }
    let mut cmd = docker::any_cli(paths)?;
    cmd.env("DOCKER_HOST", paths.docker_host()).args(args);
    Err(cmd.exec()).context("exec docker")
}

pub fn shell(paths: &Paths) -> Result<()> {
    let interactive = unsafe { libc::isatty(0) } == 1;
    let host = paths.docker_host();
    let mut cmd = docker::any_cli(paths)?;
    cmd.args(["-H", &host, "run", "--rm", if interactive { "-it" } else { "-i" }]);
    cmd.args(["--privileged", "--pid=host", "--net=host", HELPER_IMAGE]);
    cmd.args(["nsenter", "-t", "1", "-m", "-u", "-i", "-n", "-p", "--", "sh"]);
    Err(cmd.exec()).context("exec docker")
}

// ---------------------------------------------------------------- import

fn is_anonymous_volume(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Copies volume `name` with `docker cp` (the archive API) between two idle
/// helper containers, then checks that size and file count match. Streaming
/// through `docker run` stdout wedges Docker Desktop's API after a few hundred
/// MB, and a tar file on a shared Mac directory can be read stale by the guest.
fn copy_volume(src: &[&str], dst: &[&str], name: &str) -> Result<()> {
    let helper = |ctx: &[&str], mount: &str| -> Result<String> {
        docker::run_ok(&[ctx, &["create", "-v", mount, HELPER_IMAGE, "true"]].concat())
    };
    let from = helper(src, &format!("{name}:/v:ro"))?;
    let result = helper(dst, &format!("{name}:/v")).and_then(|to| {
        // The target may hold a previous partial copy (or data under --force).
        let r = docker::run_ok(&[dst, &["run", "--rm", "-v", &format!("{name}:/v"), HELPER_IMAGE, "find", "/v", "-mindepth", "1", "-delete"]].concat())
            .and_then(|_| pipe(&[src, &["cp", "-a", &format!("{from}:/v/."), "-"]].concat(), &[dst, &["cp", "-a", "-", &format!("{to}:/v")]].concat()))
            .and_then(|()| {
                let measure = |ctx: &[&str]| {
                    docker::run_ok(&[ctx, &["run", "--rm", "-v", &format!("{name}:/v:ro"), HELPER_IMAGE, "sh", "-c", "echo $(find /v | wc -l) entries, $(find /v -type f -exec stat -c %s {} + | awk '{s+=$1} END {print s+0}') bytes"]].concat())
                };
                let (a, b) = (measure(src)?, measure(dst)?);
                if a != b {
                    bail!("copy does not match: {a} at the source, {b} here");
                }
                Ok(())
            });
        let _ = docker::run(&[dst, &["rm", "-f", &to]].concat());
        r
    });
    let _ = docker::run(&[src, &["rm", "-f", &from]].concat());
    result
}

/// `docker <src...> | docker <dst...>`; returns stderr of whichever side failed.
fn pipe(src: &[&str], dst: &[&str]) -> Result<()> {
    let mut producer = docker::cli().args(src).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = producer.stdout.take().context("producer stdout")?;
    let consumer = docker::cli().args(dst).stdin(stdout).stdout(Stdio::null()).stderr(Stdio::piped()).output()?;
    let produced = producer.wait_with_output()?;
    if !produced.status.success() {
        bail!("{}", String::from_utf8_lossy(&produced.stderr).trim());
    }
    if !consumer.status.success() {
        bail!("{}", String::from_utf8_lossy(&consumer.stderr).trim());
    }
    Ok(())
}

fn ensure_image(target: &[&str], image: &str) -> Result<()> {
    let mut inspect = target.to_vec();
    inspect.extend(["image", "inspect", image]);
    if !docker::run(&inspect)?.status.success() {
        let mut pull = target.to_vec();
        pull.extend(["pull", "-q", image]);
        docker::run_ok(&pull)?;
    }
    Ok(())
}

pub fn import(paths: &Paths, from: &str, only: &[String], images: bool, volumes: bool, force: bool) -> Result<()> {
    let wanted = |name: &str| only.is_empty() || only.iter().any(|o| o == name);
    if from == CONTEXT {
        bail!("--from must be another docker context, not `{CONTEXT}`");
    }
    if !api_get(paths, "/_ping").is_ok_and(|b| b == b"OK") {
        bail!("rekcod is not running; `rekcod start` first");
    }
    let host = paths.docker_host();
    let dst: [&str; 2] = ["-H", &host];
    let src: [&str; 2] = ["--context", from];
    docker::run_ok(&[&src[..], &["version", "--format", "{{.Server.Version}}"]].concat())
        .with_context(|| format!("source context `{from}` is not reachable"))?;

    let (mut imported, mut skipped) = (0, 0);
    if images {
        let list = docker::run_ok(&[&src[..], &["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"]].concat())?;
        let mut refs: Vec<&str> = list.lines().filter(|r| !r.contains("<none>") && wanted(r)).collect();
        refs.sort();
        refs.dedup();
        println!("images: {} in {from}", refs.len());
        for (i, r) in refs.iter().enumerate() {
            let t = Instant::now();
            if docker::run(&[&dst[..], &["image", "inspect", r]].concat())?.status.success() {
                println!("  [{}/{}] {r}: already here", i + 1, refs.len());
                skipped += 1;
                continue;
            }
            let load = [&dst[..], &["load", "-q"]].concat();
            let mut result = pipe(&[&src[..], &["save", r]].concat(), &load);
            if result.is_err() {
                // containerd image stores may hold partial multi-platform indexes; export just the local platform.
                let platform = docker::run_ok(&[&src[..], &["image", "inspect", "--format", "{{.Os}}/{{.Architecture}}", r]].concat())?;
                result = pipe(&[&src[..], &["save", "--platform", &platform, r]].concat(), &load);
            }
            match result {
                Ok(()) => {
                    imported += 1;
                    println!("  [{}/{}] {r}: ok ({:.1}s)", i + 1, refs.len(), t.elapsed().as_secs_f64());
                }
                Err(e) => println!("  [{}/{}] {r}: FAILED: {e:#}", i + 1, refs.len()),
            }
        }
    }

    if volumes {
        let list = docker::run_ok(&[&src[..], &["volume", "ls", "--format", "{{.Name}}"]].concat())?;
        let names: Vec<&str> = list.lines().filter(|n| !n.is_empty()).collect();
        let anonymous = names.iter().filter(|n| is_anonymous_volume(n)).count();
        let named: Vec<&str> = names.into_iter().filter(|n| !is_anonymous_volume(n) && wanted(n)).collect();
        println!("volumes: {} named in {from} ({anonymous} anonymous skipped)", named.len());
        if !named.is_empty() {
            ensure_image(&src, HELPER_IMAGE)?;
            ensure_image(&dst, HELPER_IMAGE)?;
        }
        for name in named {
            let users = docker::run_ok(&[&src[..], &["ps", "--filter", &format!("volume={name}"), "--format", "{{.Names}}"]].concat())?;
            if !users.is_empty() && !force {
                println!("  {name}: skipped, in use by {} (stop it at the source first, or --force)", users.replace('\n', ", "));
                skipped += 1;
                continue;
            }
            if docker::run(&[&dst[..], &["volume", "inspect", name]].concat())?.status.success() {
                let mount = format!("{name}:/v:ro");
                let probe = [&dst[..], &["run", "--rm", "-v", &mount, HELPER_IMAGE, "ls", "-A", "/v"]].concat();
                if !docker::run_ok(&probe)?.is_empty() && !force {
                    println!("  {name}: skipped, already has data here (--force to overwrite)");
                    skipped += 1;
                    continue;
                }
            } else {
                let fmt = "{{.Driver}}\t{{json .Labels}}";
                let info = docker::run_ok(&[&src[..], &["volume", "inspect", "--format", fmt, name]].concat())?;
                let (driver, labels) = info.split_once('\t').unwrap_or(("local", "null"));
                if driver != "local" {
                    println!("  {name}: skipped, driver `{driver}` is not supported");
                    skipped += 1;
                    continue;
                }
                let labels: BTreeMap<String, String> = serde_json::from_str(labels).ok().flatten().unwrap_or_default();
                let mut create: Vec<String> = dst.iter().map(|s| s.to_string()).collect();
                create.extend(["volume".into(), "create".into()]);
                // Compose checks its labels before reusing a volume.
                for (k, v) in &labels {
                    create.extend(["--label".into(), format!("{k}={v}")]);
                }
                create.push(name.into());
                docker::run_ok(&create.iter().map(String::as_str).collect::<Vec<_>>())?;
            }
            let t = Instant::now();
            let result = copy_volume(&src, &dst, name);
            match result {
                Ok(()) => {
                    imported += 1;
                    println!("  {name}: ok ({:.1}s)", t.elapsed().as_secs_f64());
                }
                Err(e) => println!("  {name}: FAILED: {e:#}"),
            }
        }
    }
    println!("imported {imported}, skipped {skipped}. Containers are not copied: run `docker compose up -d` in your projects.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_volume_names() {
        assert!(is_anonymous_volume(&"a".repeat(64)));
        assert!(!is_anonymous_volume("myapp_pgdata"));
        assert!(!is_anonymous_volume(&"g".repeat(64)));
    }
}
