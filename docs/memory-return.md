# Returning guest memory to macOS

**Outcome:** elastic memory ([elastic-memory.md](elastic-memory.md)) gives memory above a
base back while the VM runs. Base RAM still can't be returned; this is what was tried.

A rekcod VM gives its RAM back to macOS only when it stops. A container that briefly uses
6 GB (DuckDB, a big build, a test run) leaves the VM holding 6 GB on the Mac until the
next `rekcod stop`. This file records what has been tried.

The memory isn't in the rekcod process. Guest RAM lives in Apple's XPC service,
`com.apple.Virtualization.VirtualMachine` (one per VM), so that's the process to measure:

```sh
footprint $(pgrep -f com.apple.Virtualization.VirtualMachine)
```

## What doesn't work

| Attempt | Result | Where |
|---|---|---|
| VZ traditional balloon | guest inflates (6.4 GB ballooned), XPC footprint stays at 8 GB | `phase2.md` |
| Balloon under `memory_pressure -l warn` | RSS falls by compression, footprint unchanged | `phase5.md` |
| `init_on_free=1` in the guest | worse: the guest touches all 8 GiB at boot | `phase5.md` |
| `madvise` on guest RAM from rekcod (macOS 27) | footprint unchanged with every advice value | below |
| Balloon allocating 16K-aligned groups of four 4K pages, so every reported host page is fully covered | footprint stays at 6.1 GB with 5 GB ballooned; the patched driver also livelocked under OOM until inflation stopped using direct reclaim | 2026-10-17 |
| Hot-adding a VZ shared memory region as guest RAM | the VM stops as soon as code runs from it | below |

## The `madvise` experiment (2026-10-16, macOS 27.0.1)

macOS 27 added custom virtio devices. A `VZCustomVirtioDevice` can call
`guestMemoryMappingAtPhysicalAddress:length:`, which returns a `VZGuestMemoryMapping` whose
`mutableBytes` points at guest RAM inside the rekcod process. The idea was to build a
balloon with free page reporting on top of that: the guest reports free 2 MB blocks, and
rekcod `madvise`s them away.

The probe was a dummy custom device (device ID `0x7e`, PCI class `0xff`, no guest driver)
added behind an env var in a throwaway VM (own `REKCOD_HOME`, 8 GiB). A privileged
container allocated and filled 2 to 3 GiB, read its physical addresses from
`/proc/self/pagemap`, and handed the 16K-aligned runs to the host. The host mapped each
run and applied the advice, then the container read every page back.

| Advice | `madvise` | XPC footprint before, after | Guest reads back |
|---|---|---|---|
| `MADV_FREE_REUSABLE`, 3 GiB | ok on all 6 runs | 3868 MB, 3873 MB | its data, unchanged |
| `MADV_FREE`, 2 GiB | ok on all 515 runs | 2651 MB, 2651 MB | its data, unchanged |
| `MADV_DONTNEED`, 2 GiB | ok on all 8 runs | 2662 MB, 2662 MB | its data, unchanged |
| write 0 to each page (control) | n/a | 2638 MB, 2641 MB | all 524288 pages zero |

The control shows the mapping really is the guest's memory: a host write showed up in
the guest. `madvise` returns success and does nothing, because the pages belong to the
XPC process's memory object and the mapping in rekcod is just another view of it. I
suspect macOS ignores `REUSABLE` on memory shared like this; I didn't find documentation
either way.

So a free-page-reporting balloon built on `VZCustomVirtioDevice` can learn which pages
are free but can't release them. Only Virtualization.framework itself can, and it has no
API for that. The probe code isn't in the tree; it was about 130 lines in
`crates/rekcod/src/memprobe.rs` plus a `setCustomVirtioDevices:` call in `vm.rs`.

## Shared memory regions (2026-10-17, macOS 27.0.1)

`VZVirtioSharedMemoryRegion` maps memory that rekcod allocates into a region the guest sees
as a PCI BAR. That memory is rekcod's, so unmapping it really frees it: with 1 GiB written by
the guest, the Mac's used memory fell 1125 MB on `unmapMemoryAtOffset` + `munmap`, with the
VM running.

What the guest can do with it, tested from a kernel module on a write-back `memremap`:

| Access | Result |
|---|---|
| load/store, unaligned load | works |
| `clear_page` (`DC ZVA`), `atomic64_inc`, `cmpxchg64`, `ldxr`/`stxr` | works |
| virtio-blk DMA into a page there | works |
| executing code there | the vCPU hangs; with `PROT_EXEC` on rekcod's side too |

Hot-adding the region with `add_memory_driver_managed` succeeds. Onlined as kernel memory the
VM survives; onlined movable, user pages and their page cache land there and the VM stops
within a second ("guest stopped on its own (Error)"), with no balloon device and with no
workload at all. Code from the page cache running there is what kills it. So the region can
hold data the guest only copies, which is what elastic memory uses it for.

Rules found on the way: `mapMemory` must run on the device's queue and only while the VM
runs ("The virtual machine is not live"), and `unmapMemoryAtOffset` fails ("An unsupported
operation") for anything but exactly what one `mapMemory` call mapped.

## What to do instead

- Cap the workload: `docker run --memory 4g`, or `SET memory_limit='4GB'` in DuckDB. That
  limits how much the VM ever touches. It returns nothing.
- `rekcod start --memory N` caps the whole VM. It applies at the next VM start.
- Stopping the VM frees everything. `rekcod stop` deletes `--rm` containers.
- Fleet apps can stop their own VMs when idle (`fleet-compute.md`, `idle_stop`).
- Use elastic memory: a small `--base-memory` and the rest elastic.
- Recheck with each macOS release: a free-page-reporting option on
  `VZVirtioTraditionalMemoryBalloonDeviceConfiguration`, or a new balloon class, would fix
  this directly.
