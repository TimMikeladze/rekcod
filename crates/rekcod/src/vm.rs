//! The Linux VM, via Apple's Virtualization.framework.
//!
//! The VM lives on its own serial dispatch queue; every call into
//! `VZVirtualMachine` is dispatched there. Async callers get results back
//! over oneshot channels, so the rest of the daemon is plain tokio.

use std::os::fd::{FromRawFd, IntoRawFd, RawFd};
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
    VZFileHandleSerialPortAttachment, VZGenericPlatformConfiguration, VZLinuxBootLoader, VZLinuxRosettaAvailability,
    VZLinuxRosettaDirectoryShare, VZMACAddress, VZMultipleDirectoryShare, VZNATNetworkDeviceAttachment, VZSharedDirectory,
    VZStorageDeviceConfiguration, VZVirtioBlockDeviceConfiguration, VZVirtioConsoleDeviceSerialPortConfiguration,
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
        let (config, rosetta) = unsafe { build_config(cfg)? };
        let config = OnQueue(config);
        let queue = DispatchQueue::new("dev.rekcod.vm", None);
        let mut vm = None;
        let q = OnQueue(&*queue);
        queue.exec_sync(|| {
            vm = Some(OnQueue(unsafe { VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), config.get(), q.get()) }));
        });
        let vm = vm.expect("exec_sync ran the closure");
        Ok(Vm { queue: queue.clone(), vm: Arc::new(vm), rosetta })
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
        Vm { queue: queue.clone(), vm: Arc::new(vm.expect("exec_sync ran the closure")), rosetta: false }
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

unsafe fn build_config(cfg: &VmConfig) -> Result<(Retained<VZVirtualMachineConfiguration>, bool)> {
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

        let net = VZVirtioNetworkDeviceConfiguration::new();
        net.setAttachment(Some(&Retained::into_super(VZNATNetworkDeviceAttachment::new())));
        let mac = VZMACAddress::initWithString(VZMACAddress::alloc(), &NSString::from_str(&cfg.mac_address))
            .with_context(|| format!("bad MAC address {}", cfg.mac_address))?;
        net.setMACAddress(&mac);
        c.setNetworkDevices(&NSArray::from_retained_slice(&[Retained::into_super(net)]));

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
        Ok((c, rosetta))
    }
}

/// Bytes a connection keeps in memory for a slow reader; past this it spills to a file.
const CONN_BUFFER_MAX: usize = 64 << 20;
/// Bytes it may spill before the reader gets an error instead.
const CONN_SPILL_MAX: u64 = 16 << 30;

enum Chunk {
    Data(Vec<u8>),
    /// This many more bytes were appended to the spill file.
    Spilled(usize),
    /// The spill file is full: the reader gets ConnectionReset, never a short clean EOF.
    Overflow,
}

/// A vsock connection that is read eagerly.
///
/// Virtualization.framework delivers guest->host vsock data with a *blocking*
/// `writev` on the vsock device's queue, and vCPUs synchronize with that queue.
/// If we stop reading one connection (e.g. a docker CLI that stopped draining a
/// log stream, so `copy_bidirectional` stops reading the guest side), the
/// write blocks, a vCPU blocks behind it, IPIs to that vCPU go unanswered, and
/// the whole guest freezes (docs/phase2b-4.md, "VM freeze"). So a task always
/// reads. A reader that falls [`CONN_BUFFER_MAX`] behind gets the rest from an
/// unlinked temp file; one that falls [`CONN_SPILL_MAX`] further behind gets an
/// error. Either way it never sees a clean EOF on a stream that was cut short.
pub struct Conn {
    rx: tokio::sync::mpsc::UnboundedReceiver<Chunk>,
    pending: Vec<u8>,
    pos: usize,
    buffered: Arc<std::sync::atomic::AtomicUsize>,
    /// The spill file, its read offset, and bytes in it not yet read.
    spill: Option<(Arc<std::fs::File>, u64, u64)>,
    spill_file: Arc<std::sync::OnceLock<Arc<std::fs::File>>>,
    write: tokio::io::WriteHalf<UnixStream>,
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

impl Conn {
    fn new(stream: UnixStream) -> Conn {
        use std::io::Write as _;
        use std::sync::atomic::Ordering;
        use tokio::io::AsyncReadExt;
        let (mut read, write) = tokio::io::split(stream);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let buffered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let spill_file: Arc<std::sync::OnceLock<Arc<std::fs::File>>> = Arc::default();
        let (counter, shared) = (buffered.clone(), spill_file.clone());
        let pump = tokio::spawn(async move {
            let mut buf = vec![0u8; 256 * 1024];
            let mut spilled: u64 = 0;
            let mut file: Option<Arc<std::fs::File>> = None;
            let discard = |mut read: tokio::io::ReadHalf<UnixStream>, mut buf: Vec<u8>| async move {
                while matches!(read.read(&mut buf).await, Ok(n) if n > 0) {}
            };
            loop {
                let n = match read.read(&mut buf).await {
                    Ok(0) | Err(_) => return, // dropping tx = EOF for the reader
                    Ok(n) => n,
                };
                // Once spilling, everything after goes to the file too, so order holds.
                if file.is_none() && counter.load(Ordering::Relaxed) + n <= CONN_BUFFER_MAX {
                    counter.fetch_add(n, Ordering::Relaxed);
                    if tx.send(Chunk::Data(buf[..n].to_vec())).is_err() {
                        return discard(read, buf).await; // reader gone
                    }
                    continue;
                }
                if file.is_none() {
                    match open_spill_file() {
                        Ok(f) => {
                            let f = Arc::new(f);
                            let _ = shared.set(f.clone());
                            file = Some(f);
                        }
                        Err(e) => {
                            eprintln!("rekcod: vsock reader stalled and spilling failed ({e}); dropping the connection");
                            let _ = tx.send(Chunk::Overflow);
                            return discard(read, buf).await;
                        }
                    }
                }
                let f = file.as_ref().expect("spill file");
                if spilled + n as u64 > CONN_SPILL_MAX || (&**f).write_all(&buf[..n]).is_err() {
                    eprintln!("rekcod: vsock reader fell {} GiB behind; dropping the connection", CONN_SPILL_MAX >> 30);
                    let _ = tx.send(Chunk::Overflow);
                    return discard(read, buf).await;
                }
                spilled += n as u64;
                if tx.send(Chunk::Spilled(n)).is_err() {
                    return discard(read, buf).await;
                }
            }
        });
        Conn { rx, pending: Vec::new(), pos: 0, buffered, spill: None, spill_file, write, pump }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl tokio::io::AsyncRead for Conn {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::os::unix::fs::FileExt;
        use std::task::Poll;
        let this = &mut *self;
        loop {
            if this.pos < this.pending.len() {
                let n = out.remaining().min(this.pending.len() - this.pos);
                out.put_slice(&this.pending[this.pos..this.pos + n]);
                this.pos += n;
                return Poll::Ready(Ok(()));
            }
            if let Some((f, off, left)) = this.spill.as_mut().filter(|s| s.2 > 0) {
                let mut tmp = vec![0u8; (*left).min(out.remaining() as u64).min(1 << 20) as usize];
                let n = f.read_at(&mut tmp, *off)?;
                if n == 0 {
                    return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "spill file truncated")));
                }
                *off += n as u64;
                *left -= n as u64;
                out.put_slice(&tmp[..n]);
                return Poll::Ready(Ok(()));
            }
            match this.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(())), // EOF
                Poll::Ready(Some(Chunk::Data(chunk))) => {
                    this.buffered.fetch_sub(chunk.len(), std::sync::atomic::Ordering::Relaxed);
                    this.pending = chunk;
                    this.pos = 0;
                }
                Poll::Ready(Some(Chunk::Spilled(n))) => {
                    if this.spill.is_none() {
                        let f = this.spill_file.get().cloned().expect("spill file set before Spilled");
                        this.spill = Some((f, 0, 0));
                    }
                    if let Some(s) = this.spill.as_mut() {
                        s.2 += n as u64;
                    }
                }
                Poll::Ready(Some(Chunk::Overflow)) => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "rekcod: reader fell too far behind the VM and the stream was cut",
                    )));
                }
            }
        }
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
        assert!(conn.spill_file.get().is_some(), "should have spilled");
        let mut got = Vec::with_capacity(total);
        conn.read_to_end(&mut got).await.unwrap();
        assert_eq!(got.len(), total);
        assert!(got.iter().enumerate().all(|(k, b)| *b as usize == (k + 1) % 251));
    }
}
