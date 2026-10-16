//! macOS guests (docs/platform.md, track 4): base images and macOS sandboxes.
//!
//! Apple's licence allows two macOS VMs per Mac, for development and testing, and
//! Virtualization.framework refuses a third. A base image is installed from Apple's restore
//! image, then set up by hand once (Setup Assistant, a `rekcod` user, Remote Login), because
//! the unattended provisioning API isn't in the Rust bindings. Sandboxes clone the base and
//! are driven over SSH with a key only rekcod has.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSArray, NSData, NSError, NSString};
use objc2_virtualization::{
    VZDiskImageStorageDeviceAttachment, VZMACAddress, VZMacAuxiliaryStorage, VZMacAuxiliaryStorageInitializationOptions,
    VZMacGraphicsDeviceConfiguration, VZMacGraphicsDisplayConfiguration, VZMacHardwareModel, VZMacKeyboardConfiguration,
    VZMacMachineIdentifier, VZMacOSBootLoader, VZMacOSInstaller, VZMacOSRestoreImage, VZMacPlatformConfiguration,
    VZMacTrackpadConfiguration, VZNATNetworkDeviceAttachment, VZVirtioBlockDeviceConfiguration, VZVirtioNetworkDeviceConfiguration,
    VZVirtualMachine, VZVirtualMachineConfiguration, VZVirtualMachineState,
};
use serde_json::json;

use crate::paths::Paths;
use crate::vm::{ns_error, url};

/// Guest account the base image must have, with Remote Login on.
pub const USER: &str = "rekcod";
/// Apple's licence: two macOS VMs per Mac (macOS SLA §2B(iii)).
pub const MAX_RUNNING: usize = 2;

/// Lets an Objective-C object cross to the thread that waits for a completion handler.
struct Sendable<T>(T);
// SAFETY: used only to hand a result from a completion handler to the waiting thread,
// which then uses it alone.
unsafe impl<T> Send for Sendable<T> {}
// SAFETY: the installer is only touched inside `vm.with`, on the VM queue.
unsafe impl<T> Sync for Sendable<T> {}

fn root(paths: &Paths) -> PathBuf {
    paths.home.join("macos")
}

fn ipsw(paths: &Paths) -> PathBuf {
    root(paths).join("restore.ipsw")
}

pub fn image_dir(paths: &Paths, name: &str) -> PathBuf {
    root(paths).join("images").join(name)
}

/// The SSH key rekcod uses for every macOS guest; created on first use.
pub fn key(paths: &Paths) -> Result<PathBuf> {
    let k = root(paths).join("id_ed25519");
    if !k.exists() {
        std::fs::create_dir_all(root(paths))?;
        let ok = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-C", "rekcod", "-f"]).arg(&k).status()?.success();
        if !ok {
            bail!("ssh-keygen failed");
        }
    }
    Ok(k)
}

/// Downloads the newest restore image this Mac supports (resumable).
pub fn fetch(paths: &Paths) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(Some(tx));
    let block = RcBlock::new(move |img: *mut VZMacOSRestoreImage, err: *mut NSError| {
        let r = if err.is_null() && !img.is_null() {
            let img = unsafe { &*img };
            let u = unsafe { img.URL().absoluteString() }.map(|s| s.to_string()).unwrap_or_default();
            Ok((u, unsafe { img.buildVersion() }.to_string()))
        } else {
            Err(anyhow!("{:#}", ns_error(err)))
        };
        if let Some(tx) = tx.lock().unwrap().take() {
            let _ = tx.send(r);
        }
    });
    unsafe { VZMacOSRestoreImage::fetchLatestSupportedWithCompletionHandler(&block) };
    let (u, build) = rx.recv_timeout(Duration::from_secs(60)).context("Apple's restore image catalog didn't answer")??;
    std::fs::create_dir_all(root(paths))?;
    let part = root(paths).join("restore.ipsw.part");
    eprintln!("rekcod: downloading macOS {build} ({u})");
    let ok =
        Command::new("curl").args(["-fL", "--retry", "10", "--retry-all-errors", "-C", "-", "-o"]).arg(&part).arg(&u).status()?.success();
    if !ok {
        bail!("download failed; run `rekcod macos fetch` again to resume");
    }
    std::fs::rename(&part, ipsw(paths))?;
    std::fs::write(root(paths).join("restore.build"), &build)?;
    println!("{}", ipsw(paths).display());
    Ok(())
}

fn load_image(path: &Path) -> Result<Retained<VZMacOSRestoreImage>> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(Some(tx));
    let block = RcBlock::new(move |img: *mut VZMacOSRestoreImage, err: *mut NSError| {
        let r = if err.is_null() && !img.is_null() {
            Ok(Sendable(unsafe { Retained::retain(img) }.expect("non-null")))
        } else {
            Err(anyhow!("{:#}", ns_error(err)))
        };
        if let Some(tx) = tx.lock().unwrap().take() {
            let _ = tx.send(r);
        }
    });
    unsafe { VZMacOSRestoreImage::loadFileURL_completionHandler(&url(path), &block) };
    Ok(rx.recv_timeout(Duration::from_secs(120)).context("loading the restore image timed out")??.0)
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct GuestConfig {
    pub cpus: usize,
    pub memory_gib: u64,
    pub mac: String,
}

fn read_config(dir: &Path) -> Result<GuestConfig> {
    Ok(serde_json::from_slice(&std::fs::read(dir.join("config.json")).with_context(|| format!("no macOS guest in {}", dir.display()))?)?)
}

fn new_mac() -> String {
    unsafe { VZMACAddress::randomLocallyAdministeredAddress().string() }.to_string()
}

fn new_machine_id(dir: &Path) -> Result<()> {
    let id = unsafe { VZMacMachineIdentifier::new() };
    std::fs::write(dir.join("machine.bin"), unsafe { id.dataRepresentation() }.to_vec())?;
    Ok(())
}

/// The VM configuration for the guest in `dir` (disk.img, aux.img, hw.bin, machine.bin).
fn configuration(dir: &Path, g: &GuestConfig) -> Result<Retained<VZVirtualMachineConfiguration>> {
    unsafe {
        let platform = VZMacPlatformConfiguration::new();
        let hw = VZMacHardwareModel::initWithDataRepresentation(
            VZMacHardwareModel::alloc(),
            &NSData::with_bytes(&std::fs::read(dir.join("hw.bin"))?),
        )
        .context("bad hardware model")?;
        if !hw.isSupported() {
            bail!("this Mac can't run the guest's hardware model");
        }
        platform.setHardwareModel(&hw);
        let id = VZMacMachineIdentifier::initWithDataRepresentation(
            VZMacMachineIdentifier::alloc(),
            &NSData::with_bytes(&std::fs::read(dir.join("machine.bin"))?),
        )
        .context("bad machine identifier")?;
        platform.setMachineIdentifier(&id);
        platform.setAuxiliaryStorage(Some(&VZMacAuxiliaryStorage::initWithURL(VZMacAuxiliaryStorage::alloc(), &url(&dir.join("aux.img")))));

        let c = VZVirtualMachineConfiguration::new();
        c.setPlatform(&Retained::into_super(platform));
        c.setBootLoader(Some(&Retained::into_super(VZMacOSBootLoader::new())));
        c.setCPUCount(g.cpus);
        c.setMemorySize(g.memory_gib << 30);
        let att = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url(&dir.join("disk.img")),
            false,
        )
        .map_err(|e| anyhow!("{}", e.localizedDescription()))?;
        let blk =
            VZVirtioBlockDeviceConfiguration::initWithAttachment(VZVirtioBlockDeviceConfiguration::alloc(), &Retained::into_super(att));
        c.setStorageDevices(&NSArray::from_retained_slice(&[Retained::into_super(blk)]));
        let net = VZVirtioNetworkDeviceConfiguration::new();
        net.setAttachment(Some(&Retained::into_super(VZNATNetworkDeviceAttachment::new())));
        let mac = VZMACAddress::initWithString(VZMACAddress::alloc(), &NSString::from_str(&g.mac)).context("bad MAC address")?;
        net.setMACAddress(&mac);
        c.setNetworkDevices(&NSArray::from_retained_slice(&[Retained::into_super(net)]));
        let gfx = VZMacGraphicsDeviceConfiguration::new();
        let display = VZMacGraphicsDisplayConfiguration::initWithWidthInPixels_heightInPixels_pixelsPerInch(
            VZMacGraphicsDisplayConfiguration::alloc(),
            1920,
            1200,
            80,
        );
        gfx.setDisplays(&NSArray::from_retained_slice(&[display]));
        c.setGraphicsDevices(&NSArray::from_retained_slice(&[Retained::into_super(gfx)]));
        c.setKeyboards(&NSArray::from_retained_slice(&[Retained::into_super(VZMacKeyboardConfiguration::new())]));
        c.setPointingDevices(&NSArray::from_retained_slice(&[Retained::into_super(VZMacTrackpadConfiguration::new())]));
        c.validateWithError().map_err(|e| anyhow!("invalid macOS VM configuration: {}", e.localizedDescription()))?;
        Ok(c)
    }
}

/// Installs the downloaded restore image into a new base image `name`.
pub fn install(paths: &Paths, name: &str, disk_gib: u64, cpus: usize, memory_gib: u64) -> Result<()> {
    crate::sandbox::check_name("macOS image", name)?;
    // A new base image has a new host key.
    let _ = std::fs::remove_file(known_hosts(paths));
    let restore = ipsw(paths);
    if !restore.exists() {
        bail!("no restore image; run `rekcod macos fetch` first");
    }
    let dir = image_dir(paths, name);
    if dir.join("config.json").exists() {
        bail!("macOS image {name} exists; `rekcod macos rm {name}` first");
    }
    std::fs::create_dir_all(&dir)?;
    let img = load_image(&restore)?;
    let req = unsafe { img.mostFeaturefulSupportedConfiguration() }.context("this Mac can't run that restore image")?;
    let hw = unsafe { req.hardwareModel() };
    unsafe {
        VZMacAuxiliaryStorage::initCreatingStorageAtURL_hardwareModel_options_error(
            VZMacAuxiliaryStorage::alloc(),
            &url(&dir.join("aux.img")),
            &hw,
            VZMacAuxiliaryStorageInitializationOptions::AllowOverwrite,
        )
        .map_err(|e| anyhow!("create auxiliary storage: {}", e.localizedDescription()))?;
    }
    std::fs::write(dir.join("hw.bin"), unsafe { hw.dataRepresentation() }.to_vec())?;
    new_machine_id(&dir)?;
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("disk.img"))?.set_len(disk_gib << 30)?;
    let g = GuestConfig {
        cpus: cpus.max(unsafe { req.minimumSupportedCPUCount() }),
        memory_gib: memory_gib.max(unsafe { req.minimumSupportedMemorySize() } >> 30),
        mac: new_mac(),
    };
    std::fs::write(dir.join("config.json"), serde_json::to_vec_pretty(&g)?)?;
    let vm = crate::vm::Vm::from_config(configuration(&dir, &g)?);
    let installer = vm.with(|v| {
        Sendable(unsafe { VZMacOSInstaller::initWithVirtualMachine_restoreImageURL(VZMacOSInstaller::alloc(), v, &url(&restore)) })
    });
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(Some(tx));
    let inst = &installer;
    vm.with(move |_| {
        let block = RcBlock::new(move |err: *mut NSError| {
            let r = if err.is_null() { Ok(()) } else { Err(anyhow!("{:#}", ns_error(err))) };
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(r);
            }
        });
        unsafe { inst.0.installWithCompletionHandler(&block) };
    });
    eprintln!("rekcod: installing macOS into {} (takes a while)", dir.display());
    let t0 = Instant::now();
    let result = loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(r) => break r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let done = vm.with(|_| unsafe { inst.0.progress().fractionCompleted() });
                eprintln!("rekcod: {:>3.0}% after {}s", done * 100.0, t0.elapsed().as_secs());
            }
            Err(e) => break Err(anyhow!("installer: {e}")),
        }
    };
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e.context("install failed"));
    }
    println!(
        "macOS image {name} installed. Finish it once by hand: `rekcod macos setup {name}` opens its screen.\n\
         In Setup Assistant create the user `{USER}`, then in System Settings turn on Remote Login\n\
         (General › Sharing) and automatic login for {USER} (Users & Groups). For screenshots and input,\n\
         also allow /usr/libexec/sshd-keygen-wrapper under Privacy & Security › Screen Recording and\n\
         Accessibility. Then run `rekcod macos authorize {name}` and shut the guest down from its Apple menu."
    );
    Ok(())
}

/// Opens the guest's screen in a window (Setup Assistant, settings). Exits when the guest shuts down.
pub fn setup(paths: &Paths, name: &str) -> Result<()> {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSTimer};
    use objc2_virtualization::VZVirtualMachineView;
    let dir = image_dir(paths, name);
    let g = read_config(&dir)?;
    if running_ip(&g.mac).is_some() && crate::cli::running_pid(&Paths { home: dir.clone() }).is_some() {
        bail!("image {name} is running");
    }
    let mtm = MainThreadMarker::new().context("setup must run on the main thread")?;
    let cfg = configuration(&dir, &g)?;
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        // The view needs a VM on the main queue.
        let vm = VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &cfg);
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));
        let style =
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable | NSWindowStyleMask::Miniaturizable;
        let window =
            NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect, style, NSBackingStoreType::Buffered, false);
        window.setReleasedWhenClosed(false);
        window.setTitle(&NSString::from_str(&format!("rekcod macOS image {name}")));
        let view = VZVirtualMachineView::new(mtm);
        view.setVirtualMachine(Some(&vm));
        view.setCapturesSystemKeys(true);
        window.setContentView(Some(&view));
        window.center();
        window.makeKeyAndOrderFront(None);
        let start = RcBlock::new(|err: *mut NSError| {
            if !err.is_null() {
                eprintln!("rekcod: start: {:#}", ns_error(err));
                std::process::exit(1);
            }
        });
        vm.startWithCompletionHandler(&start);
        let watched = vm.clone();
        let tick = RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| {
            let s = watched.state();
            if s == VZVirtualMachineState::Stopped || s == VZVirtualMachineState::Error {
                eprintln!("rekcod: guest shut down");
                std::process::exit(0);
            }
        });
        let _timer = NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &tick);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        app.run();
    }
    Ok(())
}

/// The guest's address from macOS's DHCP leases for vmnet, by MAC.
pub fn running_ip(mac: &str) -> Option<String> {
    let norm = |m: &str| m.split(':').map(|o| u8::from_str_radix(o, 16).unwrap_or(0)).collect::<Vec<u8>>();
    let want = norm(mac);
    let text = std::fs::read_to_string("/var/db/dhcpd_leases").ok()?;
    parse_leases(&text).into_iter().find(|(m, _)| norm(m) == want).map(|(_, ip)| ip)
}

/// (MAC, IP) pairs from `/var/db/dhcpd_leases`, newest first as the file lists them.
pub fn parse_leases(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in text.split('{').skip(1) {
        let field = |k: &str| block.lines().find_map(|l| l.trim().strip_prefix(&format!("{k}=")).map(|v| v.trim().to_string()));
        if let (Some(ip), Some(hw)) = (field("ip_address"), field("hw_address")) {
            out.push((hw.split_once(',').map(|(_, m)| m.to_string()).unwrap_or(hw), ip));
        }
    }
    out
}

/// Installs rekcod's key for `rekcod` in a running guest (asks for the guest password once).
pub fn authorize(paths: &Paths, name: &str) -> Result<()> {
    let g = read_config(&image_dir(paths, name))?;
    let ip = running_ip(&g.mac).context("the guest has no address yet; is it running (`rekcod macos setup`)?")?;
    let k = key(paths)?;
    let ok = Command::new("ssh-copy-id")
        .args(["-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-i"])
        .arg(k.with_extension("pub"))
        .arg(format!("{USER}@{ip}"))
        .status()?
        .success();
    if !ok {
        bail!("ssh-copy-id failed; is Remote Login on and the user called {USER}?");
    }
    std::fs::write(image_dir(paths, name).join("authorized"), b"")?;
    println!("authorized; shut the guest down from its Apple menu to finish image {name}");
    Ok(())
}

pub fn list(paths: &Paths) -> Result<()> {
    if ipsw(paths).exists() {
        println!("restore image: macOS {}", std::fs::read_to_string(root(paths).join("restore.build")).unwrap_or_default().trim());
    }
    for e in std::fs::read_dir(root(paths).join("images")).into_iter().flatten().flatten() {
        let ready = if e.path().join("authorized").exists() { "ready" } else { "needs `rekcod macos setup`" };
        println!("{:<12} {ready}", e.file_name().to_string_lossy());
    }
    Ok(())
}

pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    crate::sandbox::check_name("macOS image", name)?;
    let dir = image_dir(paths, name);
    if !dir.exists() {
        bail!("no macOS image {name}");
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

/// True when a base image is ready for sandboxes.
pub fn ready(paths: &Paths, name: &str) -> bool {
    image_dir(paths, name).join("authorized").exists()
}

// ------------------------------------------------------------- sandboxes

/// Clones base image `name` into a sandbox home with a new machine identity.
pub fn clone_image(paths: &Paths, name: &str, home: &Path, cpus: usize, memory_gib: u64) -> Result<()> {
    let src = image_dir(paths, name);
    if !ready(paths, name) {
        bail!("macOS image {name} isn't ready; see `rekcod macos ls`");
    }
    let base = read_config(&src)?;
    for f in ["disk.img", "aux.img", "hw.bin"] {
        crate::sandbox::clonefile(&src.join(f), &home.join(f))?;
    }
    new_machine_id(home)?;
    let g = GuestConfig { cpus: cpus.max(base.cpus), memory_gib: memory_gib.max(base.memory_gib), mac: new_mac() };
    std::fs::write(home.join("config.json"), serde_json::to_vec_pretty(&g)?)?;
    Ok(())
}

/// Gives a cloned guest (fork) its own identity.
pub fn reidentify(home: &Path) -> Result<()> {
    new_machine_id(home)?;
    let mut g = read_config(home)?;
    g.mac = new_mac();
    std::fs::write(home.join("config.json"), serde_json::to_vec_pretty(&g)?)?;
    Ok(())
}

/// Guest host keys seen so far. Clones share their base image's key, so the first one
/// learned for an address holds for every guest given it; another VM on the vmnet that
/// answers there with a different key is refused. `macos install` starts it over.
fn known_hosts(paths: &Paths) -> PathBuf {
    root(paths).join("known_hosts")
}

fn ssh_base(paths: &Paths, ip: &str) -> Result<Command> {
    let mut c = Command::new("ssh");
    c.args(["-q", "-o", "StrictHostKeyChecking=accept-new", "-o"])
        .arg(format!("UserKnownHostsFile={}", known_hosts(paths).display()))
        .args(["-o", "ConnectTimeout=5", "-o", "BatchMode=yes", "-i"])
        .arg(key(paths)?)
        .arg(format!("{USER}@{ip}"));
    Ok(c)
}

/// Boots the sandbox's guest if needed and waits for SSH; returns its address.
pub fn boot(paths: &Paths, home: &Path, idle_ttl: u64) -> Result<String> {
    let sb = Paths::at(home.to_path_buf())?;
    let g = read_config(home)?;
    if crate::cli::running_pid(&sb).is_none() {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(sb.daemon_log())?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args(["macos", "run"]).arg(home).args(["--idle-ttl", &idle_ttl.to_string()]);
        cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().context("spawn macOS guest")?;
    }
    let t0 = Instant::now();
    loop {
        if let Some(ip) = running_ip(&g.mac)
            && ssh_base(paths, &ip)?.arg("true").stdin(Stdio::null()).status().is_ok_and(|s| s.success())
        {
            return Ok(ip);
        }
        if t0.elapsed() > Duration::from_secs(240) {
            bail!("the macOS guest didn't answer SSH within 240s; see {}", sb.daemon_log().display());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Copies the sandbox's workspace into the guest (`~/ws`), as a tar stream over ssh.
pub fn push_ws(paths: &Paths, ip: &str, ws: &Path) -> Result<()> {
    let mut tar = Command::new("tar").args(["-C"]).arg(ws).args(["-cf", "-", "."]).stdout(Stdio::piped()).spawn()?;
    let out = tar.stdout.take().context("tar stdout")?;
    let ok = ssh_base(paths, ip)?.arg("rm -rf ~/ws && mkdir -p ~/ws && tar -C ~/ws -xf -").stdin(out).status()?.success();
    if !tar.wait()?.success() || !ok {
        bail!("copying the workspace into the guest failed");
    }
    Ok(())
}

/// Brings the guest's `~/ws` back into the sandbox's workspace, for diff and apply. The
/// guest is untrusted, so its tar stream goes into an empty directory with the Mac's bsdtar,
/// which refuses absolute paths, `..` and writing through symlinks; `apply` then refuses
/// symlinks that point out of the project. The workspace's own `.git` is kept.
pub fn pull_ws(paths: &Paths, ip: &str, ws: &Path) -> Result<()> {
    let parent = ws.parent().context("workspace has no parent")?;
    let fresh = parent.join("ws.pull");
    let old = parent.join("ws.old");
    for d in [&fresh, &old] {
        if d.exists() {
            std::fs::remove_dir_all(d)?;
        }
    }
    std::fs::create_dir_all(&fresh)?;
    let mut ssh = ssh_base(paths, ip)?.arg("tar -C ~/ws --exclude ./.git -cf - .").stdout(Stdio::piped()).spawn()?;
    let out = ssh.stdout.take().context("ssh stdout")?;
    let ok = Command::new("tar").arg("-C").arg(&fresh).args(["-xf", "-"]).stdin(out).status()?.success();
    if !ssh.wait()?.success() || !ok {
        let _ = std::fs::remove_dir_all(&fresh);
        bail!("copying the workspace out of the guest failed");
    }
    if ws.join(".git").exists() {
        std::fs::rename(ws.join(".git"), fresh.join(".git"))?;
    }
    std::fs::rename(ws, &old)?;
    std::fs::rename(&fresh, ws)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// The ssh command running `cmd` in `~/ws` (or `workdir`).
pub fn exec_command(paths: &Paths, ip: &str, workdir: Option<&str>, env: &[String], cmd: &[String], tty: bool) -> Result<Command> {
    let q = |s: &str| crate::actions::shell_quote(s);
    let mut remote = format!("cd {} && ", workdir.map(q).unwrap_or_else(|| "~/ws".into()));
    for e in env {
        remote.push_str(&format!("export {}; ", q(e)));
    }
    remote.push_str("exec ");
    remote.push_str(&cmd.iter().map(|c| q(c)).collect::<Vec<_>>().join(" "));
    let mut c = ssh_base(paths, ip)?;
    if tty {
        c.arg("-t");
    }
    c.arg(remote);
    Ok(c)
}

pub fn screenshot(paths: &Paths, ip: &str, out: &Path) -> Result<()> {
    let ok =
        ssh_base(paths, ip)?.arg("screencapture -x /tmp/rekcod-shot.png && cat /tmp/rekcod-shot.png").stdout(Stdio::piped()).output()?;
    if !ok.status.success() || ok.stdout.is_empty() {
        bail!("screencapture failed; the guest needs automatic login and Screen Recording for sshd-keygen-wrapper");
    }
    std::fs::File::create(out)?.write_all(&ok.stdout)?;
    Ok(())
}

/// The JavaScript for Automation run in the guest for `input`.
pub fn input_script(action: &[String]) -> Result<String> {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    Ok(match action {
        [a, x, y] if a == "click" => {
            let (x, y): (f64, f64) = (x.parse().context("x")?, y.parse().context("y")?);
            format!(
                "ObjC.import('CoreGraphics'); var p = $.CGPointMake({x}, {y}); \
                 [$.kCGEventLeftMouseDown, $.kCGEventLeftMouseUp].forEach(function (t) {{ \
                 $.CGEventPost($.kCGHIDEventTap, $.CGEventCreateMouseEvent(null, t, p, $.kCGMouseButtonLeft)); delay(0.05); }});"
            )
        }
        [a, text] if a == "type" => format!("Application('System Events').keystroke({});", q(text)),
        [a, k] if a == "key" => {
            let mut parts: Vec<&str> = k.split('+').collect();
            let key = parts.pop().unwrap_or("");
            let mods: Vec<String> = parts
                .iter()
                .map(|m| match *m {
                    "cmd" => "'command down'".into(),
                    "ctrl" => "'control down'".into(),
                    "alt" | "opt" => "'option down'".into(),
                    "shift" => "'shift down'".into(),
                    other => q(other),
                })
                .collect();
            let codes = [
                ("return", 36),
                ("enter", 36),
                ("tab", 48),
                ("space", 49),
                ("delete", 51),
                ("escape", 53),
                ("left", 123),
                ("right", 124),
                ("down", 125),
                ("up", 126),
            ];
            match codes.iter().find(|(n, _)| *n == key) {
                Some((_, code)) => format!("Application('System Events').keyCode({code}, {{using: [{}]}});", mods.join(", ")),
                None => format!("Application('System Events').keystroke({}, {{using: [{}]}});", q(key), mods.join(", ")),
            }
        }
        _ => bail!("input takes: click X Y | type TEXT | key [cmd+|ctrl+|alt+|shift+]KEY"),
    })
}

pub fn input(paths: &Paths, ip: &str, action: &[String]) -> Result<()> {
    let script = input_script(action)?;
    let ok = ssh_base(paths, ip)?.arg(format!("osascript -l JavaScript -e {}", crate::actions::shell_quote(&script))).status()?.success();
    if !ok {
        bail!("input failed; the guest needs Accessibility allowed for sshd-keygen-wrapper");
    }
    Ok(())
}

/// `rekcod macos run HOME`: the process that owns a macOS sandbox's VM.
pub fn run(home: PathBuf, idle_ttl: u64) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let paths = Paths::at(home.clone())?;
    let g = read_config(&home)?;
    let vm = crate::vm::Vm::from_config(configuration(&home, &g)?);
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        vm.start().await?;
        std::fs::write(paths.pid_file(), std::process::id().to_string())?;
        eprintln!("rekcod: macOS guest started ({} CPUs, {} GiB, {})", g.cpus, g.memory_gib, g.mac);
        let mut term = signal(SignalKind::terminate())?;
        let mut kill = signal(SignalKind::user_defined1())?;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let stopped = || {
            let s = vm.with(|v| unsafe { v.state() });
            s == VZVirtualMachineState::Stopped || s == VZVirtualMachineState::Error
        };
        let mut asked_to_stop: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = kill.recv() => {
                    let _ = vm.force_stop().await;
                    break;
                }
                _ = term.recv() => {
                    // The power button: macOS shuts down cleanly, then the VM stops.
                    vm.request_stop();
                    let t0 = Instant::now();
                    while !stopped() && t0.elapsed() < Duration::from_secs(60) {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    if !stopped() {
                        let _ = vm.force_stop().await;
                    }
                    break;
                }
                _ = tick.tick() => {
                    if stopped() {
                        break;
                    }
                    if let Some(at) = asked_to_stop
                        && at.elapsed() > Duration::from_secs(60)
                    {
                        // The guest ignored the power button (a dialog, a hung app).
                        eprintln!("rekcod: guest didn't shut down within 60s, forcing it off");
                        let _ = vm.force_stop().await;
                        break;
                    }
                    if idle_ttl > 0
                        && asked_to_stop.is_none()
                        && let Ok(used) = std::fs::metadata(home.join("run/last-used")).and_then(|m| m.modified())
                        && used.elapsed().is_ok_and(|e| e.as_secs() > idle_ttl)
                    {
                        eprintln!("rekcod: idle for {idle_ttl}s, shutting down");
                        vm.request_stop();
                        asked_to_stop = Some(Instant::now());
                    }
                }
            }
        }
        let _ = std::fs::remove_file(paths.pid_file());
        eprintln!("rekcod: macOS guest stopped");
        anyhow::Ok(())
    })?;
    let _ = json!({});
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_and_input() {
        let text = "{\n\tname=rekcod\n\tip_address=192.168.64.7\n\thw_address=1,a:b:c:d:e:f\n\tlease=0x1\n}\n{\n\tname=x\n\tip_address=192.168.64.3\n\thw_address=1,1e:2:3:4:5:6\n}\n";
        let l = parse_leases(text);
        assert_eq!(l, vec![("a:b:c:d:e:f".into(), "192.168.64.7".into()), ("1e:2:3:4:5:6".into(), "192.168.64.3".into())]);
        assert!(input_script(&["click".into(), "10".into(), "20".into()]).unwrap().contains("CGPointMake(10, 20)"));
        assert_eq!(input_script(&["type".into(), "hi \"x\"".into()]).unwrap(), r#"Application('System Events').keystroke("hi \"x\"");"#);
        assert_eq!(
            input_script(&["key".into(), "cmd+return".into()]).unwrap(),
            "Application('System Events').keyCode(36, {using: ['command down']});"
        );
        assert!(input_script(&["dance".into()]).is_err());
    }
}
