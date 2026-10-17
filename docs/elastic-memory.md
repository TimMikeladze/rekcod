# Elastic memory

**Status:** built 2026-10-17, macOS 27 only. What was tried before this, and why it failed,
is in `memory-return.md`.

A rekcod VM keeps every page of RAM the guest has touched until it stops. After a 6 GB
DuckDB query or a big build, the Mac was short 6 GB until `rekcod stop`. Elastic memory
gives the part above a fixed base back while the VM keeps running.

```
$ rekcod start --memory 8 --base-memory 2
$ rekcod status
  vm        6 CPUs, 8192 MiB
  memory    2048 MiB RAM + 6144 MiB elastic, 0.2 GiB of it on the Mac now; macOS gets elastic memory back when the VM frees it
```

## Sizing

`--memory N` is still how much the guest can use. rekcod splits it into base RAM and
elastic memory:

| `--memory` | base RAM | elastic |
|---|---|---|
| 4 GiB or less | all of it | none |
| 8 GiB (the default on a 16 GiB+ Mac) | 4 GiB | 4 GiB |
| 16 GiB | 8 GiB | 8 GiB |

Base is `max(min(memory, 4 GiB), memory / 2)` (`rekcod_proto::split_memory`). `--base-memory
N` sets it instead and is saved in `~/.rekcod/config.json` like `--memory`. An elastic part
under 512 MiB isn't worth a device, so that VM is all RAM. Before macOS 27 all of `--memory`
is RAM, as it always was. Sandboxes and fleet instances get the same split.

## What it costs

The guest sees base RAM in `MemTotal`; elastic memory is swap (`/dev/rswap`). Memory above
base RAM goes through the swap path, which is a `memcpy` into memory the Mac maps in, not a
disk:

| 2 GiB base, 3.5 GB touched | write pass | read pass |
|---|---|---|
| elastic | 1.3 s | 1.7 s |
| 6 GiB of RAM | 0.8 s | 0.3 s |

Fine for bursts. A working set that keeps cycling through more than base RAM is slower; give
that workload `--base-memory` its size. Tools that size themselves from `MemTotal` (DuckDB's
default `memory_limit` is 80% of it) use less and spill to their own temp files.

## Measured

2 GiB base + 6 GiB elastic, a container touching 5 GB and exiting, twice in a row:

| | VM resident | Mac used | elastic on the Mac |
|---|---|---|---|
| during the burst | 8610 MB | 48011 MB | 6128 MiB |
| 15 s after it exited | 2766 MB | 41163 MB | 272 MiB |
| second burst, 15 s after | 2998 MB | 42989 MB | 411 MiB |

What stays held afterwards is chunks that still hold a few swap pages of long-running
processes. Activity Monitor's Memory column (footprint) doesn't count elastic memory at all;
`rekcod status` does.

## How it works

1. **Device** (`crates/rekcod/src/elastic.rs`). A custom virtio device (`VZCustomVirtioDevice`,
   ID 63, one request queue) with one shared memory region the size of the elastic part.
   rekcod reserves that much address space with `mmap` and, once the VM runs, maps it into
   the region in 4 MiB chunks; macOS only backs pages the guest writes. When every chunk is
   mapped, it sets `ready` in the device config (`le64 chunk_size, le32 ready`).
2. **Driver** (`guest/kernel/elastic/rekcod_elastic.c`, built into the guest kernel by
   `guest/build.sh`). It `memremap`s the region write-back and registers `/dev/rswap`, a
   bio-based block device with discard. It tracks which 4K pages hold data, and per chunk a
   count and a state: `ACTIVE` or `RELEASED`.
3. **Boot** (`boot.sh`): `mkswap /dev/rswap`, `swapon -dpages`, so freed swap clusters are
   discarded. The region is fresh memory every boot.
4. **Release.** Every 5 s the driver collects chunks that were written, hold no data, have no
   write in flight and stayed that way for a whole interval, marks them `RELEASED` and sends
   `le32 op=1, le32 count, le32 chunk[count]`. For each chunk rekcod unmaps it from the guest,
   puts fresh pages at the same address with `mmap(MAP_FIXED)` and maps it in again; macOS
   frees the old pages right away. It answers `le32 status` once the whole batch is remapped.
5. **Reuse.** A write to a `RELEASED` chunk first sends `op=2` and waits for the answer. The
   driver holds its request lock from marking chunks released until rekcod answers, so a
   reuse can't overtake the release it follows. rekcod has nothing to do for it: released
   chunks are already fresh memory.

## Why swap and not RAM

The region can't be guest RAM. Hot-adding it with memory hotplug works until a program's
code lands there: Virtualization.framework won't execute from shared memory regions, and the
VM stops ("guest stopped on its own (Error)"). Loads, stores, atomics, exclusives, `DC ZVA`
and virtio DMA into it all work (`memory-return.md`). Swap only ever copies pages in and out,
so it never executes from the region.

Two framework rules shaped the host side. The region can only be mapped while the VM runs,
and only from the device's queue. And `unmapMemoryAtOffset` only takes back exactly what one
`mapMemory` call mapped, which is why the region is mapped chunk by chunk.
`madvise(MADV_FREE_REUSABLE)` on region pages left the VM's resident memory where it was
(6437 MB before, 6453 MB after), which is why release remaps.

## Limits

- macOS 27 only (`VZCustomVirtioDevice`). Older macOS boots all of `--memory` as RAM.
- Base RAM behaves as before: touched once, held until `rekcod stop`.
- `KERNEL=alpine` guests (`guest/build.sh`) don't have the driver; the device sits unused.
- Releases come 5 to 15 s after the guest frees swap, not at once.
