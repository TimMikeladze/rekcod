//! Elastic memory (docs/elastic-memory.md): a custom virtio device whose shared memory
//! region is memory this process maps in. The guest swaps to it and tells us, chunk by
//! chunk, when it no longer holds data there; `MADV_FREE_REUSABLE` then gives those pages
//! back to macOS while the VM keeps running. Needs macOS 27 (`VZCustomVirtioDevice`).
//!
//! The classes aren't in objc2-virtualization 0.3 yet, so they're reached by name.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSData, NSError};
use rekcod_proto::{ELASTIC_CHUNK, ELASTIC_DEVICE_ID, ELASTIC_OP_RELEASE, ELASTIC_OP_REUSE};

/// Same line format as the daemon's `log!`: this runs on framework queues.
fn log(msg: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    eprintln!("[{}.{:03}] {msg}", t.as_secs(), t.subsec_millis());
}

/// Device config the guest reads: `le64 chunk_size, le32 ready, le32 reserved`.
fn config_bytes(ready: bool) -> [u8; 16] {
    let mut c = [0u8; 16];
    c[..8].copy_from_slice(&ELASTIC_CHUNK.to_le_bytes());
    c[8..12].copy_from_slice(&u32::from(ready).to_le_bytes());
    c
}

/// A request's chunk list, checked against the region: `(op, chunks)`.
fn parse_request(header: &[u8], body: &[u8], chunks: u64) -> Result<(u32, Vec<u64>)> {
    let op = u32::from_le_bytes(header.get(..4).ok_or_else(|| anyhow!("short header"))?.try_into()?);
    let count = u32::from_le_bytes(header.get(4..8).ok_or_else(|| anyhow!("short header"))?.try_into()?) as usize;
    if op != ELASTIC_OP_RELEASE && op != ELASTIC_OP_REUSE {
        bail!("unknown op {op}");
    }
    if body.len() != count * 4 {
        bail!("{count} chunks but {} bytes", body.len());
    }
    let list: Vec<u64> = body.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c) as u64).collect();
    if let Some(bad) = list.iter().find(|&&c| c >= chunks) {
        bail!("chunk {bad} outside the region ({chunks} chunks)");
    }
    Ok((op, list))
}

/// Chunks the guest has nothing mapped at: a RELEASE unmapped them and putting pages back
/// failed. They are never reported usable until a REUSE has mapped them in again.
type Lost = Mutex<std::collections::BTreeSet<u64>>;

struct Inner {
    size: u64,
    /// This process's mapping of the region.
    addr: usize,
    queue: DispatchRetained<DispatchQueue>,
    device: Mutex<Option<Retained<AnyObject>>>,
    lost: Arc<Lost>,
}
// SAFETY: the device is only messaged on its own serial queue; `addr` is our mapping.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RekcodElasticDevice"]
    #[ivars = Arc<Inner>]
    struct Delegate;

    impl Delegate {
        /// VZCustomVirtioDeviceConfigurationDelegate, on the VM queue at VM creation.
        #[unsafe(method(customVirtioConfiguration:didCreateDevice:))]
        fn did_create(&self, _config: &AnyObject, device: &AnyObject) {
            let inner = self.ivars().clone();
            let Some(device) = (unsafe { Retained::retain(device as *const AnyObject as *mut AnyObject) }) else { return };
            unsafe {
                let _: () = msg_send![&*device, setDelegate: self];
            }
            *inner.device.lock().unwrap() = Some(device);
        }

        /// VZCustomVirtioDeviceDelegate, on the device queue.
        #[unsafe(method(customVirtioDevice:didReceiveNotificationForQueue:))]
        fn notified(&self, device: &AnyObject, queue: &AnyObject) {
            let inner = self.ivars();
            loop {
                let element: Option<Retained<AnyObject>> = unsafe { msg_send![queue, nextElement] };
                let Some(element) = element else { break };
                match read_request(inner, &element) {
                    Ok((op, chunks)) if !chunks.is_empty() => {
                        let regions: Retained<NSArray<AnyObject>> = unsafe { msg_send![device, sharedMemoryRegions] };
                        let Some(region) = regions.firstObject() else {
                            reply(&element, 1);
                            continue;
                        };
                        let region: Rc<dyn Region> = Rc::new(VzRegion { region, addr: inner.addr });
                        let answer = Box::new(move |status| reply(&element, status));
                        if op == ELASTIC_OP_RELEASE {
                            release(region, inner.lost.clone(), chunks, answer);
                        } else {
                            reuse(region, inner.lost.clone(), chunks, answer);
                        }
                    }
                    Ok(_) => reply(&element, 0),
                    Err(e) => {
                        log(&format!("elastic memory: bad request: {e:#}"));
                        reply(&element, 1);
                    }
                }
            }
        }
    }
);

fn read_exact(element: &AnyObject, len: usize) -> Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let mut err: *mut NSError = std::ptr::null_mut();
    let data: Option<Retained<NSData>> = unsafe { msg_send![element, readBytesWithExactLength: len, error: &mut err] };
    data.map(|d| d.to_vec()).ok_or_else(|| anyhow!("read {len} bytes from the request"))
}

fn read_request(inner: &Inner, element: &AnyObject) -> Result<(u32, Vec<u64>)> {
    let header = read_exact(element, 8)?;
    let count = u32::from_le_bytes(header[4..8].try_into()?) as usize;
    let available: usize = unsafe { msg_send![element, readBuffersAvailableByteCount] };
    if count * 4 > available {
        bail!("{count} chunks but {available} bytes");
    }
    let body = read_exact(element, count * 4)?;
    parse_request(&header, &body, inner.size / ELASTIC_CHUNK)
}

/// Answers the guest and hands the element back.
fn reply(element: &AnyObject, status: u32) {
    unsafe {
        let data = NSData::with_bytes(&status.to_le_bytes());
        let mut err: *mut NSError = std::ptr::null_mut();
        let _: bool = msg_send![element, writeData: &*data, error: &mut err];
        let _: () = msg_send![element, returnToQueue];
    }
}

/// What RELEASE and REUSE do to the shared memory region, chunk by chunk. A trait so the
/// bookkeeping can be tested without a VM. Completions run on the device queue.
trait Region {
    /// Unmaps chunk `c` from the guest.
    fn unmap(&self, c: u64, done: Box<dyn FnOnce(Result<(), String>)>);
    /// Replaces our pages behind chunk `c` with fresh, unbacked ones.
    fn fresh(&self, c: u64) -> Result<(), String>;
    /// Maps our pages behind chunk `c` into the guest.
    fn map(&self, c: u64, done: Box<dyn FnOnce(Result<(), String>)>);
}

/// The device's region, and our mapping of it at `addr`.
struct VzRegion {
    region: Retained<AnyObject>,
    addr: usize,
}

/// A completion handler that hands the framework's error, if any, to `done`.
fn completion(done: Box<dyn FnOnce(Result<(), String>)>) -> RcBlock<dyn Fn(*mut NSError)> {
    let done = Cell::new(Some(done));
    RcBlock::new(move |e: *mut NSError| {
        if let Some(done) = done.take() {
            done(unsafe { e.as_ref() }.map_or(Ok(()), |e| Err(e.localizedDescription().to_string())));
        }
    })
}

impl Region for VzRegion {
    fn unmap(&self, c: u64, done: Box<dyn FnOnce(Result<(), String>)>) {
        let block = completion(done);
        unsafe {
            let _: () = msg_send![&*self.region, unmapMemoryAtOffset: c * ELASTIC_CHUNK, size: ELASTIC_CHUNK, completionHandler: &*block];
        }
    }

    fn fresh(&self, c: u64) -> Result<(), String> {
        let p = self.addr + (c * ELASTIC_CHUNK) as usize;
        let fresh = unsafe {
            libc::mmap(
                p as *mut libc::c_void,
                ELASTIC_CHUNK as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        if fresh == libc::MAP_FAILED { Err(std::io::Error::last_os_error().to_string()) } else { Ok(()) }
    }

    fn map(&self, c: u64, done: Box<dyn FnOnce(Result<(), String>)>) {
        let block = completion(done);
        let p = (self.addr + (c * ELASTIC_CHUNK) as usize) as *mut libc::c_void;
        unsafe {
            let _: () =
                msg_send![&*self.region, mapMemory: p, atOffset: c * ELASTIC_CHUNK, size: ELASTIC_CHUNK, completionHandler: &*block];
        }
    }
}

/// Runs `answer` once `pending` chunks are done.
struct Batch {
    pending: Cell<usize>,
    answer: Cell<Option<Box<dyn FnOnce()>>>,
}

impl Batch {
    fn new(pending: usize, answer: Box<dyn FnOnce()>) -> Rc<Batch> {
        Rc::new(Batch { pending: pending.into(), answer: Some(answer).into() })
    }

    fn done(&self) {
        self.pending.set(self.pending.get() - 1);
        if self.pending.get() == 0
            && let Some(answer) = self.answer.take()
        {
            answer();
        }
    }
}

fn quarantine(lost: &Lost, c: u64, err: &str) {
    lost.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(c);
    log(&format!("elastic memory: release: {err}; chunk {c} is quarantined until the guest reuses it"));
}

/// Gives the chunks' pages back to macOS: each is unmapped from the guest, replaced with
/// fresh (unbacked) pages here and mapped in again. The guest sent RELEASE only for chunks
/// it doesn't touch until we answer, which happens once every chunk is done.
/// (`MADV_FREE_REUSABLE` on pages the guest has mapped doesn't free them.)
///
/// The guest takes a failure to mean the chunks are still its own and goes on writing to
/// them, so the answer is 1 only when every failed chunk is still mapped as before (its
/// unmap failed). A chunk left with nothing mapped is quarantined in `lost` and the answer
/// is 0: the guest then counts them all as released and sends REUSE before writing to one.
fn release(region: Rc<dyn Region>, lost: Arc<Lost>, chunks: Vec<u64>, answer: Box<dyn FnOnce(u32)>) {
    let (kept, gone) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let batch = Batch::new(chunks.len(), {
        let (kept, gone) = (kept.clone(), gone.clone());
        Box::new(move || answer(u32::from(kept.get() && !gone.get())))
    });
    for c in chunks {
        if lost.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&c) {
            // Already has nothing mapped; only a guest that ignored an earlier answer asks.
            gone.set(true);
            batch.done();
            continue;
        }
        let (region2, lost, batch, kept, gone) = (region.clone(), lost.clone(), batch.clone(), kept.clone(), gone.clone());
        region.unmap(
            c,
            Box::new(move |r| {
                if let Err(e) = r {
                    log(&format!("elastic memory: release: unmap chunk {c}: {e}"));
                    kept.set(true);
                    return batch.done();
                }
                if let Err(e) = region2.fresh(c) {
                    quarantine(&lost, c, &format!("replace chunk {c}: {e}"));
                    gone.set(true);
                    return batch.done();
                }
                region2.map(
                    c,
                    Box::new(move |r| {
                        if let Err(e) = r {
                            quarantine(&lost, c, &format!("map chunk {c}: {e}"));
                            gone.set(true);
                        }
                        batch.done();
                    }),
                );
            }),
        );
    }
}

/// REUSE: the guest is about to write to these chunks again. A released one already has
/// fresh pages mapped; a quarantined one is mapped in again first. The answer is 1 while
/// any of them still has nothing mapped, and the guest then doesn't write to it.
fn reuse(region: Rc<dyn Region>, lost: Arc<Lost>, chunks: Vec<u64>, answer: Box<dyn FnOnce(u32)>) {
    let todo: std::collections::BTreeSet<u64> = {
        let lost = lost.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        chunks.into_iter().filter(|c| lost.contains(c)).collect()
    };
    if todo.is_empty() {
        return answer(0);
    }
    let failed = Rc::new(Cell::new(false));
    let batch = Batch::new(todo.len(), {
        let failed = failed.clone();
        Box::new(move || answer(u32::from(failed.get())))
    });
    for c in todo {
        if let Err(e) = region.fresh(c) {
            log(&format!("elastic memory: reuse: replace chunk {c}: {e}"));
            failed.set(true);
            batch.done();
            continue;
        }
        let (batch, lost, failed) = (batch.clone(), lost.clone(), failed.clone());
        region.map(
            c,
            Box::new(move |r| {
                match r {
                    Ok(()) => {
                        lost.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&c);
                        log(&format!("elastic memory: chunk {c} is mapped again"));
                    }
                    Err(e) => {
                        log(&format!("elastic memory: reuse: map chunk {c}: {e}"));
                        failed.set(true);
                    }
                }
                batch.done();
            }),
        );
    }
}

/// Bytes of the region macOS currently backs: what elastic memory costs the Mac now.
fn resident(inner: &Inner) -> u64 {
    let page = 16384usize;
    let mut v = vec![0 as libc::c_char; inner.size as usize / page];
    if unsafe { libc::mincore(inner.addr as *mut libc::c_void, inner.size as usize, v.as_mut_ptr()) } != 0 {
        return 0;
    }
    v.iter().filter(|b| **b as i32 & libc::MINCORE_INCORE != 0).count() as u64 * page as u64
}

/// Maps our memory into the region one chunk at a time (the framework only unmaps exactly
/// what one call mapped, and RELEASE unmaps single chunks), then tells the guest.
fn map_region(inner: &Arc<Inner>, device: &AnyObject) {
    let regions: Retained<NSArray<AnyObject>> = unsafe { msg_send![device, sharedMemoryRegions] };
    let Some(region) = regions.firstObject() else {
        log("elastic memory: device has no shared memory region");
        return;
    };
    let chunks = inner.size / ELASTIC_CHUNK;
    let device = SendObj(unsafe { Retained::retain(device as *const AnyObject as *mut AnyObject) }.expect("non-null"));
    let pending = std::rc::Rc::new(std::cell::Cell::new(chunks));
    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let device = std::rc::Rc::new(device);
    for c in 0..chunks {
        let off = c * ELASTIC_CHUNK;
        let (pending, failed, device) = (pending.clone(), failed.clone(), device.clone());
        let done = RcBlock::new(move |e: *mut NSError| {
            if let Some(e) = unsafe { e.as_ref() }
                && !failed.replace(true)
            {
                log(&format!("elastic memory: map chunk {c}: {}", e.localizedDescription()));
            }
            pending.set(pending.get() - 1);
            if pending.get() > 0 || failed.get() {
                return;
            }
            let config = specific_config(true);
            let updated = RcBlock::new(|e: *mut NSError| {
                if let Some(e) = unsafe { e.as_ref() } {
                    log(&format!("elastic memory: ready flag: {}", e.localizedDescription()));
                }
            });
            unsafe {
                let _: () = msg_send![device.get(), updateDeviceSpecificConfiguration: &*config, completionHandler: &*updated];
            }
        });
        unsafe {
            let p = (inner.addr + off as usize) as *mut libc::c_void;
            let _: () = msg_send![&*region, mapMemory: p, atOffset: off, size: ELASTIC_CHUNK, completionHandler: &*done];
        }
    }
}

fn specific_config(ready: bool) -> Retained<AnyObject> {
    let cls = AnyClass::get(c"VZVirtioDeviceSpecificConfiguration").expect("checked in configuration()");
    let data = NSData::with_bytes(&config_bytes(ready));
    unsafe {
        let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
        msg_send![alloc, initWithConfigurationData: &*data]
    }
}

/// The elastic device of one VM. Keep it as long as the VM: the framework holds the
/// delegate weakly.
pub struct Elastic {
    _delegate: Retained<Delegate>,
    inner: Arc<Inner>,
}
// SAFETY: see Inner; the delegate is only messaged by the framework on the device queue.
unsafe impl Send for Elastic {}
unsafe impl Sync for Elastic {}

impl Elastic {
    /// Bytes of elastic memory macOS backs right now.
    pub fn held(&self) -> u64 {
        resident(&self.inner)
    }

    /// Maps the region in and tells the guest. The VM must be running: the framework
    /// refuses before that.
    pub fn map(&self) {
        let Some(device) = self.inner.device.lock().unwrap().clone() else {
            log("elastic memory: device was never created");
            return;
        };
        // The region may only be touched on the device queue.
        let (inner, device) = (self.inner.clone(), SendObj(device));
        self.inner.queue.exec_async(move || map_region(&inner, device.get()));
    }
}

impl Drop for Elastic {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.inner.addr as *mut libc::c_void, self.inner.size as usize) };
    }
}

/// Whether this macOS has custom virtio devices (macOS 27).
pub fn available() -> bool {
    AnyClass::get(c"VZCustomVirtioDeviceConfiguration").is_some()
}

/// Whether the guest kernel has the driver (built in from `guest/kernel/elastic`). Without
/// it the guest would get only base RAM and a device it can't use, so the VM stays all RAM.
pub fn guest_supports(kernel: &std::path::Path) -> bool {
    std::fs::read(kernel).is_ok_and(|image| image.windows(DRIVER_NAME.len()).any(|w| w == DRIVER_NAME))
}

/// The driver's name, as it appears in the kernel image.
const DRIVER_NAME: &[u8] = b"rekcod_elastic";

/// A device configuration exposing `size_mib` of elastic memory, and its owner.
pub fn configuration(size_mib: u64) -> Result<(Retained<AnyObject>, Elastic)> {
    let size = size_mib << 20;
    if size == 0 || !size.is_multiple_of(ELASTIC_CHUNK) {
        bail!("elastic memory must be a non-zero multiple of {} MiB", ELASTIC_CHUNK >> 20);
    }
    let get = |name: &std::ffi::CStr| AnyClass::get(name).ok_or_else(|| anyhow!("{} needs macOS 27", name.to_string_lossy()));
    let config_cls = get(c"VZCustomVirtioDeviceConfiguration")?;
    let provider_cls = get(c"VZCustomVirtioDeviceDelegateProvider")?;
    let region_cls = get(c"VZVirtioSharedMemoryRegionConfiguration")?;
    get(c"VZVirtioDeviceSpecificConfiguration")?;

    // macOS only backs the pages the guest writes.
    let addr = unsafe {
        libc::mmap(std::ptr::null_mut(), size as usize, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_ANON | libc::MAP_PRIVATE, -1, 0)
    };
    if addr == libc::MAP_FAILED {
        bail!("reserve {size_mib} MiB of elastic memory: {}", std::io::Error::last_os_error());
    }
    let inner = Arc::new(Inner {
        size,
        addr: addr as usize,
        queue: DispatchQueue::new("dev.rekcod.elastic", None),
        device: Mutex::default(),
        lost: Arc::default(),
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc().set_ivars(inner.clone())), init] };
    let elastic = Elastic { _delegate: delegate.clone(), inner: inner.clone() };
    unsafe {
        let config: Retained<AnyObject> = msg_send![config_cls, new];
        let _: () = msg_send![&*config, setDeviceID: ELASTIC_DEVICE_ID];
        // "Other memory controller": nothing in the guest binds it by class.
        let _: () = msg_send![&*config, setPCIClassID: 0x05_u8];
        let _: () = msg_send![&*config, setPCISubclassID: 0x80_u8];
        let _: () = msg_send![&*config, setVirtioQueueCount: 1_u16];
        let specific = specific_config(false);
        let _: () = msg_send![&*config, setDeviceSpecificConfiguration: &*specific];
        let alloc: Allocated<AnyObject> = msg_send![region_cls, alloc];
        let region: Retained<AnyObject> = msg_send![alloc, initWithRegionID: 0_u8, size: size];
        let regions = NSArray::from_retained_slice(&[region]);
        let _: () = msg_send![&*config, setSharedMemoryRegions: &*regions];
        let alloc: Allocated<AnyObject> = msg_send![provider_cls, alloc];
        let provider: Retained<AnyObject> = msg_send![alloc, initWithDeviceQueue: &*inner.queue, delegate: &*delegate];
        let _: () = msg_send![&*config, setProvider: &*provider];
        Ok((config, elastic))
    }
}

struct SendObj(Retained<AnyObject>);
unsafe impl Send for SendObj {}
unsafe impl Sync for SendObj {}

impl SendObj {
    // A method call makes the closure capture the wrapper, not the inner field.
    fn get(&self) -> &AnyObject {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(op: u32, chunks: &[u32]) -> (Vec<u8>, Vec<u8>) {
        let mut h = op.to_le_bytes().to_vec();
        h.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
        (h, chunks.iter().flat_map(|c| c.to_le_bytes()).collect())
    }

    #[test]
    fn parses_requests_inside_the_region_only() {
        let (h, b) = req(ELASTIC_OP_RELEASE, &[0, 3]);
        assert_eq!(parse_request(&h, &b, 4).unwrap(), (ELASTIC_OP_RELEASE, vec![0, 3]));
        let (h, b) = req(ELASTIC_OP_REUSE, &[4]);
        assert!(parse_request(&h, &b, 4).is_err(), "chunk past the end");
        let (h, b) = req(9, &[0]);
        assert!(parse_request(&h, &b, 4).is_err(), "unknown op");
        let (h, _) = req(ELASTIC_OP_RELEASE, &[0, 1]);
        assert!(parse_request(&h, &[0; 4], 4).is_err(), "count disagrees with body");
    }

    #[test]
    fn only_kernels_with_the_driver_get_elastic_memory() {
        let dir = std::env::temp_dir().join(format!("rekcod-elastic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (with, without) = (dir.join("with"), dir.join("without"));
        std::fs::write(&with, b"\0\0driver rekcod_elastic\0").unwrap();
        std::fs::write(&without, b"\0\0an older kernel\0").unwrap();
        assert!(guest_supports(&with));
        assert!(!guest_supports(&without));
        assert!(!guest_supports(&dir.join("missing")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A region whose operations finish at once; `fail_*` chunks fail that step.
    #[derive(Default)]
    struct Fake {
        /// Chunks mapped in the guest.
        mapped: std::cell::RefCell<std::collections::BTreeSet<u64>>,
        fail_unmap: std::cell::RefCell<std::collections::BTreeSet<u64>>,
        fail_fresh: std::cell::RefCell<std::collections::BTreeSet<u64>>,
        fail_map: std::cell::RefCell<std::collections::BTreeSet<u64>>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl Region for Fake {
        fn unmap(&self, c: u64, done: Box<dyn FnOnce(Result<(), String>)>) {
            self.calls.borrow_mut().push(format!("unmap {c}"));
            let fail = self.fail_unmap.borrow().contains(&c);
            if !fail {
                self.mapped.borrow_mut().remove(&c);
            }
            done(if fail { Err("busy".into()) } else { Ok(()) });
        }
        fn fresh(&self, c: u64) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("fresh {c}"));
            if self.fail_fresh.borrow().contains(&c) { Err("no memory".into()) } else { Ok(()) }
        }
        fn map(&self, c: u64, done: Box<dyn FnOnce(Result<(), String>)>) {
            self.calls.borrow_mut().push(format!("map {c}"));
            let fail = self.fail_map.borrow().contains(&c);
            if !fail {
                self.mapped.borrow_mut().insert(c);
            }
            done(if fail { Err("refused".into()) } else { Ok(()) });
        }
    }

    #[test]
    fn a_chunk_whose_remap_failed_is_never_handed_back_as_usable() {
        use std::collections::BTreeSet;
        let fake = Rc::new(Fake::default());
        fake.mapped.borrow_mut().extend(0..4);
        let lost: Arc<Lost> = Arc::default();
        let answer: Rc<Cell<Option<u32>>> = Rc::default();
        let answered = || {
            let a = answer.clone();
            Box::new(move |s| a.set(Some(s))) as Box<dyn FnOnce(u32)>
        };
        let lost_now = || lost.lock().unwrap().clone();

        // Chunk 1 can't be mapped back in. Answering 1 would have the guest write to it.
        fake.fail_map.borrow_mut().insert(1);
        release(fake.clone(), lost.clone(), vec![0, 1], answered());
        assert_eq!(answer.take(), Some(0));
        assert_eq!(lost_now(), BTreeSet::from([1]));
        assert!(!fake.mapped.borrow().contains(&1));
        // REUSE maps it in again first, and fails while that does.
        reuse(fake.clone(), lost.clone(), vec![1], answered());
        assert_eq!(answer.take(), Some(1));
        assert_eq!(lost_now(), BTreeSet::from([1]));
        fake.fail_map.borrow_mut().clear();
        reuse(fake.clone(), lost.clone(), vec![1], answered());
        assert_eq!(answer.take(), Some(0));
        assert!(lost_now().is_empty() && fake.mapped.borrow().contains(&1));

        // The same when our fresh pages can't be put in place.
        fake.fail_fresh.borrow_mut().insert(2);
        release(fake.clone(), lost.clone(), vec![2], answered());
        assert_eq!((answer.take(), lost_now()), (Some(0), BTreeSet::from([2])));
        fake.fail_fresh.borrow_mut().clear();
        reuse(fake.clone(), lost.clone(), vec![2], answered());
        assert_eq!((answer.take(), lost_now()), (Some(0), BTreeSet::new()));

        // A failed unmap leaves the chunk as it was: the guest may keep it.
        fake.fail_unmap.borrow_mut().insert(3);
        release(fake.clone(), lost.clone(), vec![0, 3], answered());
        assert_eq!(answer.take(), Some(1));
        assert!(lost_now().is_empty() && fake.mapped.borrow().is_superset(&BTreeSet::from([0, 3])));
        // Unless another chunk in the batch lost its mapping.
        fake.fail_map.borrow_mut().insert(0);
        release(fake.clone(), lost.clone(), vec![0, 3], answered());
        assert_eq!((answer.take(), lost_now()), (Some(0), BTreeSet::from([0])));

        // A quarantined chunk isn't unmapped again; other chunks' REUSE costs nothing.
        fake.calls.borrow_mut().clear();
        release(fake.clone(), lost.clone(), vec![0], answered());
        reuse(fake.clone(), lost.clone(), vec![3], answered());
        assert_eq!(answer.take(), Some(0));
        assert!(fake.calls.borrow().is_empty(), "{:?}", fake.calls.borrow());
    }

    #[test]
    fn config_carries_chunk_size_and_ready() {
        let c = config_bytes(true);
        assert_eq!(u64::from_le_bytes(c[..8].try_into().unwrap()), ELASTIC_CHUNK);
        assert_eq!(u32::from_le_bytes(c[8..12].try_into().unwrap()), 1);
        assert_eq!(&config_bytes(false)[8..12], &[0, 0, 0, 0]);
    }
}
