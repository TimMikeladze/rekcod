//! The Linux VM, via Apple's Virtualization.framework.
//!
//! The VM lives on its own serial dispatch queue; every call into
//! `VZVirtualMachine` is dispatched there. Async callers get results back
//! over oneshot channels, so the rest of the daemon is plain tokio.

use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSDictionary, NSError, NSFileHandle, NSString, NSURL};
use objc2_virtualization::{
    VZDirectorySharingDeviceConfiguration, VZDiskImageCachingMode, VZDiskImageStorageDeviceAttachment, VZDiskImageSynchronizationMode,
    VZFileHandleNetworkDeviceAttachment, VZFileHandleSerialPortAttachment, VZGenericPlatformConfiguration, VZLinuxBootLoader,
    VZLinuxRosettaAvailability, VZLinuxRosettaDirectoryShare, VZMACAddress, VZMultipleDirectoryShare, VZNATNetworkDeviceAttachment,
    VZSharedDirectory, VZStorageDeviceConfiguration, VZVirtioBlockDeviceConfiguration, VZVirtioConsoleDeviceSerialPortConfiguration,
    VZVirtioEntropyDeviceConfiguration, VZVirtioFileSystemDeviceConfiguration, VZVirtioNetworkDeviceConfiguration,
    VZVirtioSocketConnection, VZVirtioSocketDevice, VZVirtioSocketDeviceConfiguration, VZVirtioTraditionalMemoryBalloonDevice,
    VZVirtioTraditionalMemoryBalloonDeviceConfiguration, VZVirtualMachine, VZVirtualMachineConfiguration, VZVirtualMachineState,
};
use tokio::net::UnixStream;
use tokio::sync::oneshot;

pub struct VmConfig {
    pub cpus: usize,
    pub memory_mib: u64,
    pub kernel: PathBuf,
    /// None for kernels that mount the root disk themselves (docs/custom-kernel.md).
    pub initrd: Option<PathBuf>,
    pub cmdline: String,
    pub root_disk: PathBuf,
    pub data_disk: PathBuf,
    pub mac_address: String,
    pub console_log: PathBuf,
    /// Name in the guest's `mac` virtio-fs share -> host directory.
    pub shares: Vec<(String, PathBuf)>,
    pub net: Net,
    /// Swap backed by memory macOS gets back (docs/elastic-memory.md), on top of
    /// `memory_mib` of RAM. 0 for none; ignored before macOS 27.
    pub elastic_mib: u64,
}

pub enum Net {
    /// Virtualization.framework's NAT (the main VM).
    Nat,
    /// One end of a datagram socketpair; the daemon serves the other (`netstack.rs`).
    Socket(OwnedFd),
    /// No network card (offline sandboxes).
    None,
}

/// Wrapper for Objective-C objects we only touch on the VM queue.
struct OnQueue<T>(T);
// SAFETY: the wrapped objects are only dereferenced inside closures running on
// the VM's serial dispatch queue (or before the VM exists).
unsafe impl<T> Send for OnQueue<T> {}
unsafe impl<T> Sync for OnQueue<T> {}

impl<T> OnQueue<T> {
    // A method call makes closures capture the wrapper, not the inner field.
    fn get(&self) -> &T {
        &self.0
    }
}

#[derive(Clone)]
pub struct Vm {
    queue: DispatchRetained<DispatchQueue>,
    vm: Arc<OnQueue<Retained<VZVirtualMachine>>>,
    pub rosetta: bool,
    /// The elastic-memory device, kept for the VM's lifetime.
    elastic: Option<Arc<crate::elastic::Elastic>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Stopped,
    Error,
    Other,
}

pub fn max_cpus() -> usize {
    unsafe { VZVirtualMachineConfiguration::maximumAllowedCPUCount() }
}

impl Vm {
    pub fn create(cfg: &VmConfig) -> Result<Vm> {
        let (config, rosetta, elastic) = unsafe { build_config(cfg)? };
        let config = OnQueue(config);
        let queue = DispatchQueue::new("dev.rekcod.vm", None);
        let mut vm = None;
        let q = OnQueue(&*queue);
        queue.exec_sync(|| {
            vm = Some(OnQueue(unsafe { VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), config.get(), q.get()) }));
        });
        let vm = vm.expect("exec_sync ran the closure");
        Ok(Vm { queue: queue.clone(), vm: Arc::new(vm), rosetta, elastic: elastic.map(Arc::new) })
    }

    /// A VM from a ready configuration (macOS guests, `macos.rs`).
    pub fn from_config(config: Retained<VZVirtualMachineConfiguration>) -> Vm {
        let config = OnQueue(config);
        let queue = DispatchQueue::new("dev.rekcod.vm", None);
        let mut vm = None;
        let q = OnQueue(&*queue);
        queue.exec_sync(|| {
            vm = Some(OnQueue(unsafe { VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), config.get(), q.get()) }));
        });
        Vm { queue: queue.clone(), vm: Arc::new(vm.expect("exec_sync ran the closure")), rosetta: false, elastic: None }
    }

    /// Runs `f` with the VM on its queue.
    pub fn with<R: Send>(&self, f: impl FnOnce(&VZVirtualMachine) -> R + Send) -> R {
        let vm = self.vm.clone();
        let mut out = None;
        self.queue.exec_sync(|| out = Some(f(&vm.0)));
        out.expect("exec_sync ran the closure")
    }

    /// Asks the guest to shut down (the power button); false if it can't be asked.
    pub fn request_stop(&self) -> bool {
        self.with(|vm| unsafe { vm.canRequestStop() && vm.requestStopWithError().is_ok() })
    }

    /// Bytes of elastic memory macOS backs now; None without elastic memory.
    pub fn elastic_held(&self) -> Option<u64> {
        self.elastic.as_ref().map(|e| e.held())
    }

    /// Maps the elastic memory in (docs/elastic-memory.md); call once the VM runs.
    pub fn map_elastic(&self) {
        if let Some(e) = &self.elastic {
            e.map();
        }
    }

    pub async fn start(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let tx = OnQueue(Mutex::new(Some(tx)));
        let vm = self.vm.clone();
        self.queue.exec_async(move || {
            let block = RcBlock::new(move |err: *mut NSError| {
                let res = if err.is_null() { Ok(()) } else { Err(ns_error(err)) };
                if let Some(tx) = tx.0.lock().unwrap().take() {
                    let _ = tx.send(res);
                }
            });
            unsafe { vm.0.startWithCompletionHandler(&block) };
        });
        rx.await.context("VM start callback dropped")?.context("start VM")
    }

    /// Hard power-off, for when the guest ignores a shutdown request.
    pub async fn force_stop(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let tx = OnQueue(Mutex::new(Some(tx)));
        let vm = self.vm.clone();
        self.queue.exec_async(move || {
            let block = RcBlock::new(move |err: *mut NSError| {
                let res = if err.is_null() { Ok(()) } else { Err(ns_error(err)) };
                if let Some(tx) = tx.0.lock().unwrap().take() {
                    let _ = tx.send(res);
                }
            });
            unsafe { vm.0.stopWithCompletionHandler(&block) };
        });
        rx.await.context("VM stop callback dropped")?
    }

    pub fn state(&self) -> State {
        let mut state = VZVirtualMachineState::Stopped;
        let vm = &self.vm;
        self.queue.exec_sync(|| state = unsafe { vm.0.state() });
        match state {
            VZVirtualMachineState::Running => State::Running,
            VZVirtualMachineState::Stopped => State::Stopped,
            VZVirtualMachineState::Error => State::Error,
            _ => State::Other,
        }
    }

    /// Memory the guest may use; the balloon returns the rest to macOS.
    pub fn set_balloon_target(&self, bytes: u64) {
        let vm = self.vm.clone();
        self.queue.exec_async(move || unsafe {
            let devices = vm.0.memoryBalloonDevices();
            if let Some(Ok(balloon)) = devices.firstObject().map(|d| d.downcast::<VZVirtioTraditionalMemoryBalloonDevice>()) {
                balloon.setTargetVirtualMachineMemorySize(bytes);
            }
        });
    }

    /// Opens a vsock connection to `port` in the guest. See [`Conn`] for why
    /// it is always drained.
    pub async fn connect(&self, port: u32) -> Result<Conn> {
        Ok(Conn::new(self.connect_raw(port).await?))
    }

    async fn connect_raw(&self, port: u32) -> Result<UnixStream> {
        let (tx, rx) = oneshot::channel::<Result<RawFd>>();
        let tx = OnQueue(Mutex::new(Some(tx)));
        let vm = self.vm.clone();
        self.queue.exec_async(move || {
            let send = move |res: Result<RawFd>| {
                // If the caller gave up, don't leak the connection's fd.
                if let Some(tx) = tx.0.lock().unwrap().take()
                    && let Err(Ok(fd)) = tx.send(res)
                {
                    unsafe { libc::close(fd) };
                }
            };
            let devices = unsafe { vm.0.socketDevices() };
            let Some(device) = devices.firstObject() else {
                return send(Err(anyhow!("VM has no vsock device")));
            };
            let Ok(device) = device.downcast::<VZVirtioSocketDevice>() else {
                return send(Err(anyhow!("unexpected vsock device type")));
            };
            let block = RcBlock::new(move |conn: *mut VZVirtioSocketConnection, err: *mut NSError| {
                if conn.is_null() {
                    return send(Err(ns_error(err)));
                }
                // The connection object closes its fd when released; keep a dup.
                let fd = unsafe { libc::dup((*conn).fileDescriptor()) };
                send(if fd < 0 { Err(std::io::Error::last_os_error().into()) } else { Ok(fd) });
            });
            unsafe { device.connectToPort_completionHandler(port, &block) };
        });
        let fd = rx.await.context("vsock callback dropped")??;
        let std = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        std.set_nonblocking(true)?;
        Ok(UnixStream::from_std(std)?)
    }
}

pub(crate) fn ns_error(err: *mut NSError) -> anyhow::Error {
    if err.is_null() {
        return anyhow!("unknown Virtualization.framework error");
    }
    let err = unsafe { &*err };
    anyhow!("{} (code {})", err.localizedDescription(), err.code())
}

pub(crate) fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

/// Guest flushes map to `fsync(2)` on the image, like native macOS apps get.
/// VZ's default (`Full`) turns each one into `F_FULLFSYNC`, a flush to the
/// physical media that costs milliseconds and made Postgres commits ~18x slower.
/// `REKCOD_DISK_SYNC=full` restores it.
fn sync_mode() -> VZDiskImageSynchronizationMode {
    match std::env::var("REKCOD_DISK_SYNC").as_deref() {
        Ok("full") => VZDiskImageSynchronizationMode::Full,
        Ok("none") => VZDiskImageSynchronizationMode::None,
        _ => VZDiskImageSynchronizationMode::Fsync,
    }
}

unsafe fn disk(path: &Path) -> Result<Retained<VZStorageDeviceConfiguration>> {
    let attachment = unsafe {
        VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url(path),
            false,
            VZDiskImageCachingMode::Automatic,
            sync_mode(),
        )
    }
    .map_err(|e| anyhow!("disk {}: {}", path.display(), e.localizedDescription()))?;
    let dev = unsafe { VZVirtioBlockDeviceConfiguration::initWithAttachment(VZVirtioBlockDeviceConfiguration::alloc(), &attachment) };
    Ok(Retained::into_super(dev))
}

unsafe fn build_config(cfg: &VmConfig) -> Result<(Retained<VZVirtualMachineConfiguration>, bool, Option<crate::elastic::Elastic>)> {
    unsafe {
        let boot = VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &url(&cfg.kernel));
        if let Some(initrd) = &cfg.initrd {
            boot.setInitialRamdiskURL(Some(&url(initrd)));
        }
        boot.setCommandLine(&NSString::from_str(&cfg.cmdline));

        let c = VZVirtualMachineConfiguration::new();
        c.setBootLoader(Some(&Retained::into_super(boot)));
        c.setPlatform(&Retained::into_super(VZGenericPlatformConfiguration::new()));
        c.setCPUCount(cfg.cpus);
        c.setMemorySize(cfg.memory_mib * 1024 * 1024);

        // vda = root, vdb = data (order matters to the guest's boot script).
        c.setStorageDevices(&NSArray::from_retained_slice(&[disk(&cfg.root_disk)?, disk(&cfg.data_disk)?]));

        let attachment = match &cfg.net {
            Net::Nat => Some(Retained::into_super(VZNATNetworkDeviceAttachment::new())),
            Net::Socket(fd) => {
                let fd = fd.try_clone().context("dup the network socket")?;
                let handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(NSFileHandle::alloc(), fd.into_raw_fd(), true);
                Some(Retained::into_super(VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                    VZFileHandleNetworkDeviceAttachment::alloc(),
                    &handle,
                )))
            }
            Net::None => None,
        };
        if let Some(attachment) = attachment {
            let net = VZVirtioNetworkDeviceConfiguration::new();
            net.setAttachment(Some(&attachment));
            let mac = VZMACAddress::initWithString(VZMACAddress::alloc(), &NSString::from_str(&cfg.mac_address))
                .with_context(|| format!("bad MAC address {}", cfg.mac_address))?;
            net.setMACAddress(&mac);
            c.setNetworkDevices(&NSArray::from_retained_slice(&[Retained::into_super(net)]));
        }

        // One virtio-fs device exposing the Mac directories by name.
        let names: Vec<Retained<NSString>> = cfg.shares.iter().map(|(n, _)| NSString::from_str(n)).collect();
        let dirs: Vec<Retained<VZSharedDirectory>> =
            cfg.shares.iter().map(|(_, p)| VZSharedDirectory::initWithURL_readOnly(VZSharedDirectory::alloc(), &url(p), false)).collect();
        let name_refs: Vec<&NSString> = names.iter().map(|n| &**n).collect();
        let dir_refs: Vec<&VZSharedDirectory> = dirs.iter().map(|d| &**d).collect();
        let dict = NSDictionary::from_slices(&name_refs, &dir_refs);
        let multi = VZMultipleDirectoryShare::initWithDirectories(VZMultipleDirectoryShare::alloc(), &dict);
        let mac_fs = VZVirtioFileSystemDeviceConfiguration::initWithTag(
            VZVirtioFileSystemDeviceConfiguration::alloc(),
            &NSString::from_str(rekcod_proto::MAC_SHARE_TAG),
        );
        mac_fs.setShare(Some(&Retained::into_super(multi)));
        let mut shares: Vec<Retained<VZDirectorySharingDeviceConfiguration>> = vec![Retained::into_super(mac_fs)];

        let rosetta = VZLinuxRosettaDirectoryShare::availability() == VZLinuxRosettaAvailability::Installed;
        if rosetta {
            let share = VZLinuxRosettaDirectoryShare::initWithError(VZLinuxRosettaDirectoryShare::alloc())
                .map_err(|e| anyhow!("rosetta share: {}", e.localizedDescription()))?;
            let fs = VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &NSString::from_str(rekcod_proto::ROSETTA_SHARE_TAG),
            );
            fs.setShare(Some(&Retained::into_super(share)));
            shares.push(Retained::into_super(fs));
        }
        c.setDirectorySharingDevices(&NSArray::from_retained_slice(&shares));
        // Elastic memory (docs/elastic-memory.md): swap the guest can hand back to macOS.
        let elastic = if cfg.elastic_mib > 0 && crate::elastic::available() {
            let (device, elastic) = crate::elastic::configuration(cfg.elastic_mib)?;
            let devices = NSArray::from_retained_slice(&[device]);
            let _: () = objc2::msg_send![&*c, setCustomVirtioDevices: &*devices];
            Some(elastic)
        } else {
            None
        };

        c.setSocketDevices(&NSArray::from_retained_slice(&[Retained::into_super(VZVirtioSocketDeviceConfiguration::new())]));
        c.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(VZVirtioEntropyDeviceConfiguration::new())]));
        // Debug knob (VM freeze bisect, docs/phase2b-4.md): REKCOD_NO_BALLOON=1.
        if std::env::var_os("REKCOD_NO_BALLOON").is_none() {
            c.setMemoryBalloonDevices(&NSArray::from_retained_slice(&[Retained::into_super(
                VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new(),
            )]));
        }

        // Kernel console (hvc0) appends to the console log.
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cfg.console_log)
            .with_context(|| format!("open {}", cfg.console_log.display()))?;
        let handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(NSFileHandle::alloc(), log.into_raw_fd(), true);
        let attachment = VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
            VZFileHandleSerialPortAttachment::alloc(),
            None,
            Some(&handle),
        );
        let console = VZVirtioConsoleDeviceSerialPortConfiguration::new();
        console.setAttachment(Some(&Retained::into_super(attachment)));
        c.setSerialPorts(&NSArray::from_retained_slice(&[Retained::into_super(console)]));

        c.validateWithError().map_err(|e| anyhow!("invalid VM config: {}", e.localizedDescription()))?;
        Ok((c, rosetta, elastic))
    }
}

/// Unread bytes a connection keeps in memory for a slow reader; past this it spills to files.
const CONN_BUFFER_MAX: usize = 64 << 20;
/// Unread bytes it may have spilled before the reader gets an error instead.
const CONN_SPILL_MAX: u64 = 16 << 30;
/// A spill file takes this much before the next one starts. Each is deleted once read,
/// so a reader that is never quite caught up doesn't keep every byte it was ever sent.
const CONN_SPILL_FILE: u64 = 256 << 20;

/// Where a connection's unread bytes go. Parameters so tests can scale them down.
#[derive(Clone, Copy)]
struct Limits {
    read: usize,
    memory: usize,
    spill: u64,
    file: u64,
    open_spill: fn() -> std::io::Result<std::fs::File>,
}

const LIMITS: Limits =
    Limits { read: 256 * 1024, memory: CONN_BUFFER_MAX, spill: CONN_SPILL_MAX, file: CONN_SPILL_FILE, open_spill: open_spill_file };

enum Seg {
    Mem {
        data: Vec<u8>,
        pos: usize,
    },
    /// `len` unread bytes at `off` in a spill file.
    Spill {
        file: Arc<std::fs::File>,
        off: u64,
        len: u64,
    },
}

/// The bytes read from the guest and not yet by the caller, in order.
#[derive(Default)]
struct Queue {
    segs: std::collections::VecDeque<Seg>,
    /// Unread bytes in memory, and in spill files.
    memory: usize,
    spilled: u64,
    /// The spill file new data goes to, and its length. Dropped when the spill drains.
    file: Option<(Arc<std::fs::File>, u64)>,
    /// After the last segment: `Ok` for a clean EOF, or the error that ended the stream.
    end: Option<std::io::Result<()>>,
    waker: Option<std::task::Waker>,
}

impl Queue {
    fn lock(queue: &Mutex<Queue>) -> std::sync::MutexGuard<'_, Queue> {
        queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Runs `f` on the queue, then wakes the reader.
fn update(queue: &Mutex<Queue>, f: impl FnOnce(&mut Queue)) {
    let mut q = Queue::lock(queue);
    f(&mut q);
    let waker = q.waker.take();
    drop(q);
    if let Some(w) = waker {
        w.wake();
    }
}

/// A vsock connection that is read eagerly.
///
/// Virtualization.framework delivers guest->host vsock data with a *blocking*
/// `writev` on the vsock device's queue, and vCPUs synchronize with that queue.
/// If we stop reading one connection (e.g. a docker CLI that stopped draining a
/// log stream, so `copy_bidirectional` stops reading the guest side), the
/// write blocks, a vCPU blocks behind it, IPIs to that vCPU go unanswered, and
/// the whole guest freezes (docs/phase2b-4.md, "VM freeze"). So a task always
/// reads. A reader more than [`CONN_BUFFER_MAX`] behind gets the rest from unlinked
/// temp files; one more than [`CONN_SPILL_MAX`] further behind gets an error. Both
/// limits count unread bytes, so a reader that catches up is back on the memory path.
/// The reader sees EOF only where the guest closed the stream. A read error, a failed
/// spill or falling too far behind reaches it as an error, after the bytes before it.
pub struct Conn {
    queue: Arc<Mutex<Queue>>,
    write: Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>,
    pump: tokio::task::JoinHandle<()>,
}

fn open_spill_file() -> std::io::Result<std::fs::File> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("rekcod-conn-{}-{n}", std::process::id()));
    let f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(f)
}

fn spill_failed(e: std::io::Error) -> std::io::Error {
    eprintln!("rekcod: vsock reader stalled and spilling failed ({e}); dropping the connection");
    std::io::Error::new(e.kind(), format!("rekcod: couldn't spill a stalled stream to a temp file: {e}"))
}

/// Queues `data` for the reader: in memory while that stays within `limits.memory`,
/// otherwise in a spill file.
fn push(queue: &Mutex<Queue>, data: &[u8], limits: &Limits) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let n = data.len() as u64;
    let (file, off) = {
        let mut q = Queue::lock(queue);
        if q.memory + data.len() <= limits.memory {
            drop(q);
            update(queue, |q| {
                q.memory += data.len();
                q.segs.push_back(Seg::Mem { data: data.to_vec(), pos: 0 });
            });
            return Ok(());
        }
        if q.spilled + n > limits.spill {
            eprintln!("rekcod: vsock reader fell {} MiB behind; dropping the connection", (limits.memory as u64 + limits.spill) >> 20);
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "rekcod: reader fell too far behind the VM and the stream was cut",
            ));
        }
        if q.file.as_ref().is_none_or(|(_, len)| len + n > limits.file) {
            q.file = Some((Arc::new((limits.open_spill)().map_err(spill_failed)?), 0));
        }
        let (f, len) = q.file.as_mut().expect("just set");
        *len += n;
        (f.clone(), *len - n)
    };
    file.write_all_at(data, off).map_err(spill_failed)?;
    update(queue, |q| {
        q.spilled += n;
        match q.segs.back_mut() {
            Some(Seg::Spill { file: f, off: o, len }) if Arc::ptr_eq(f, &file) && *o + *len == off => *len += n,
            _ => q.segs.push_back(Seg::Spill { file, off, len: n }),
        }
    });
    Ok(())
}

async fn pump<R: tokio::io::AsyncRead + Unpin>(mut read: R, queue: Arc<Mutex<Queue>>, limits: Limits) {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; limits.read];
    let end = loop {
        match read.read(&mut buf).await {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = push(&queue, &buf[..n], &limits) {
                    break Err(e);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    let cut = end.is_err();
    update(&queue, |q| q.end = Some(end));
    // Nobody reads what follows, but the guest's writes must never block (see Conn).
    while cut && matches!(read.read(&mut buf).await, Ok(n) if n > 0) {}
}

impl Conn {
    fn new(stream: UnixStream) -> Conn {
        Conn::with_limits(stream, LIMITS)
    }

    fn with_limits<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + 'static>(stream: S, limits: Limits) -> Conn {
        let (read, write) = tokio::io::split(stream);
        let queue: Arc<Mutex<Queue>> = Arc::default();
        let pump = tokio::spawn(pump(read, queue.clone(), limits));
        Conn { queue, write: Box::new(write), pump }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl tokio::io::AsyncRead for Conn {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::os::unix::fs::FileExt;
        use std::task::Poll;
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut q = Queue::lock(&self.queue);
        let (file, off, want) = match q.segs.front_mut() {
            Some(Seg::Mem { data, pos }) => {
                let n = out.remaining().min(data.len() - *pos);
                out.put_slice(&data[*pos..*pos + n]);
                *pos += n;
                if *pos == data.len() {
                    q.segs.pop_front();
                }
                q.memory -= n;
                return Poll::Ready(Ok(()));
            }
            Some(Seg::Spill { file, off, len }) => (file.clone(), *off, (*len).min(out.remaining() as u64).min(1 << 20) as usize),
            None => {
                return match q.end.take() {
                    None => {
                        q.waker = Some(cx.waker().clone());
                        Poll::Pending
                    }
                    Some(Ok(())) => {
                        q.end = Some(Ok(()));
                        Poll::Ready(Ok(()))
                    }
                    Some(Err(e)) => {
                        q.end = Some(Err(std::io::Error::new(e.kind(), e.to_string())));
                        Poll::Ready(Err(e))
                    }
                };
            }
        };
        // Only this side removes segments, so the front one stays put while it's read.
        drop(q);
        let mut tmp = vec![0u8; want];
        let n = file.read_at(&mut tmp, off)?;
        if n == 0 {
            return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "spill file truncated")));
        }
        out.put_slice(&tmp[..n]);
        let mut q = Queue::lock(&self.queue);
        if let Some(Seg::Spill { off, len, .. }) = q.segs.front_mut() {
            *off += n as u64;
            *len -= n as u64;
            if *len == 0 {
                q.segs.pop_front();
            }
        }
        q.spilled -= n as u64;
        if q.spilled == 0 {
            // Everything spilled has been read; closing the unlinked file frees its space.
            q.file = None;
        }
        Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for Conn {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.write).poll_write(cx, buf)
    }
    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.write).poll_flush(cx)
    }
    fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.write).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_stalled_reader_gets_every_byte_in_order() {
        let (guest, host) = UnixStream::pair().unwrap();
        let mut conn = Conn::new(host);
        // More than the memory cap, written while nobody reads: the rest spills to a file.
        let total = CONN_BUFFER_MAX + (8 << 20);
        let writer = tokio::spawn(async move {
            let mut guest = guest;
            let mut i: u32 = 0;
            let mut sent = 0;
            while sent < total {
                let chunk: Vec<u8> = (0..65536)
                    .map(|_| {
                        i = i.wrapping_add(1);
                        (i % 251) as u8
                    })
                    .collect();
                guest.write_all(&chunk).await.unwrap();
                sent += chunk.len();
            }
        });
        writer.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(Queue::lock(&conn.queue).spilled > 0, "should have spilled");
        let mut got = Vec::with_capacity(total);
        conn.read_to_end(&mut got).await.unwrap();
        assert_eq!(got.len(), total);
        assert!(got.iter().enumerate().all(|(k, b)| *b as usize == (k + 1) % 251));
    }

    /// Reads `chunks`, then fails with `err`, or EOF without one. Writes go nowhere.
    struct Scripted {
        chunks: std::collections::VecDeque<Vec<u8>>,
        err: Option<std::io::Error>,
    }

    impl Scripted {
        fn new(chunks: &[&[u8]], err: Option<std::io::Error>) -> Scripted {
            Scripted { chunks: chunks.iter().map(|c| c.to_vec()).collect(), err }
        }
    }

    impl tokio::io::AsyncRead for Scripted {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            out: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if let Some(c) = self.chunks.pop_front() {
                out.put_slice(&c);
                return std::task::Poll::Ready(Ok(()));
            }
            std::task::Poll::Ready(self.err.take().map_or(Ok(()), Err))
        }
    }

    impl tokio::io::AsyncWrite for Scripted {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Reads until EOF or an error: (bytes, how it ended).
    async fn drain(conn: &mut Conn) -> (Vec<u8>, std::io::Result<()>) {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match conn.read(&mut buf).await {
                Ok(0) => return (got, Ok(())),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => return (got, Err(e)),
            }
        }
    }

    /// Waits until the pump has queued `bytes` unread bytes, or the stream ended.
    async fn queued(conn: &Conn, bytes: u64) {
        for _ in 0..2000 {
            {
                let q = Queue::lock(&conn.queue);
                if q.memory as u64 + q.spilled >= bytes || q.end.is_some() {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the pump never queued {bytes} bytes");
    }

    /// 1 KiB in memory, 4 KiB in spill files of 2 KiB, read 256 bytes at a time.
    fn small(open_spill: fn() -> std::io::Result<std::fs::File>) -> Limits {
        Limits { read: 256, memory: 1024, spill: 4096, file: 2048, open_spill }
    }

    /// The next `n` bytes of a stream whose byte `k` is `k % 251`.
    fn pattern(at: &mut usize, n: usize) -> Vec<u8> {
        let out = (*at..*at + n).map(|k| (k % 251) as u8).collect();
        *at += n;
        out
    }

    #[tokio::test]
    async fn a_transport_error_reaches_the_reader_after_its_bytes() {
        use std::io::{Error, ErrorKind};
        let reset = Scripted::new(&[b"pay", b"load"], Some(Error::from(ErrorKind::ConnectionReset)));
        let (got, end) = drain(&mut Conn::with_limits(reset, LIMITS)).await;
        assert_eq!(got, b"payload");
        assert_eq!(end.unwrap_err().kind(), ErrorKind::ConnectionReset);

        let device = Scripted::new(&[b"payload"], Some(Error::from_raw_os_error(libc::EIO)));
        let mut conn = Conn::with_limits(device, LIMITS);
        let (got, end) = drain(&mut conn).await;
        assert_eq!(got, b"payload");
        assert_eq!(end.unwrap_err().raw_os_error(), Some(libc::EIO));
        // Still an error on the next read, never a late EOF.
        assert!(conn.read(&mut [0u8; 8]).await.is_err());

        let (got, end) = drain(&mut Conn::with_limits(Scripted::new(&[b"payload"], None), LIMITS)).await;
        assert_eq!((got.as_slice(), end.is_ok()), (&b"payload"[..], true));
    }

    #[tokio::test]
    async fn a_failed_spill_is_an_error_not_eof() {
        let full: fn() -> std::io::Result<std::fs::File> = || Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
        // Opens, but every write fails.
        let read_only: fn() -> std::io::Result<std::fs::File> = || std::fs::File::open("/dev/null");
        for open in [full, read_only] {
            let mut at = 0;
            let data = pattern(&mut at, 2048);
            let chunks: Vec<&[u8]> = data.chunks(256).collect();
            let mut conn = Conn::with_limits(Scripted::new(&chunks, None), small(open));
            queued(&conn, u64::MAX).await;
            let (got, end) = drain(&mut conn).await;
            assert_eq!(got, data[..1024]);
            let err = end.unwrap_err();
            assert!(err.to_string().contains("couldn't spill"), "{err}");
        }
    }

    #[tokio::test]
    async fn a_caught_up_reader_is_never_cut() {
        let (mut guest, host) = UnixStream::pair().unwrap();
        let mut conn = Conn::with_limits(host, small(open_spill_file));
        let (mut sent, mut read) = (0, 0);
        // A stall: 3 KiB while nobody reads, 2 KiB of it spilled.
        guest.write_all(&pattern(&mut sent, 3072)).await.unwrap();
        queued(&conn, 3072).await;
        assert_eq!(Queue::lock(&conn.queue).spilled, 2048);
        let mut got = vec![0u8; 3072];
        conn.read_exact(&mut got).await.unwrap();
        assert_eq!(got, pattern(&mut read, 3072));
        {
            let q = Queue::lock(&conn.queue);
            assert!(q.spilled == 0 && q.file.is_none(), "the drained spill file is closed");
        }
        // Caught up since: each round spills 1 KiB again, 16 KiB in all, 4x the spill limit.
        for _ in 0..16 {
            guest.write_all(&pattern(&mut sent, 2048)).await.unwrap();
            queued(&conn, 2048).await;
            let mut got = vec![0u8; 2048];
            conn.read_exact(&mut got).await.unwrap();
            assert_eq!(got, pattern(&mut read, 2048));
        }
        drop(guest);
        assert_eq!(drain(&mut conn).await.1.ok(), Some(()));
    }

    #[tokio::test]
    async fn a_reader_too_far_behind_gets_an_error() {
        let (mut guest, host) = UnixStream::pair().unwrap();
        let mut conn = Conn::with_limits(host, small(open_spill_file));
        let mut sent = 0;
        // 1 KiB in memory and 4 KiB spilled are all it keeps unread.
        guest.write_all(&pattern(&mut sent, 1024 + 4096 + 256)).await.unwrap();
        queued(&conn, u64::MAX).await;
        // The guest's writes still never block.
        let more = tokio::time::timeout(std::time::Duration::from_secs(5), guest.write_all(&[0u8; 256 << 10])).await;
        assert!(matches!(more, Ok(Ok(()))), "{more:?}");
        let (got, end) = drain(&mut conn).await;
        assert_eq!(got, pattern(&mut 0, 1024 + 4096));
        assert_eq!(end.unwrap_err().kind(), std::io::ErrorKind::ConnectionReset);
    }
}
