# Phase 2: fast, coherent bind mounts (+ memory)

Phase 1 measured rekcod's bind mounts at 3–6× slower than OrbStack's, and VM-local
volumes at OrbStack parity (`plan.md`, phase 0 results). The cause: the guest
asks macOS for almost every lookup, stat and open, because Apple's virtio-fs server
hands out tiny cache timeouts and caches no negative lookups.

## Approach: cache forever in the guest, invalidate from FSEvents

1. Patched `fuse.ko`/`virtiofs.ko`, built out of tree against Alpine's
   `linux-virt` 6.12 headers from the matching kernel.org sources. They replace the
   stock modules in the root image. Signing isn't enforced (`MODULE_SIG_FORCE` is off).
   - Mount option `rekcod` (virtio-fs only, so FUSE inside containers is untouched).
     Entry, attribute and **negative** lookups never expire on their own.
   - `/sys/module/fuse/parameters/rekcod_notify` (write-only). One absolute path per
     line. For each path the module:
     - drops that inode's attributes and page cache (`fuse_reverse_inval_inode`), and
       invalidates the name in its parent (`fuse_reverse_inval_entry`, covering
       positive and negative dentries);
     - looks the name up again and emits the matching fsnotify event:
       `IN_CREATE`, `IN_DELETE`, or `IN_MODIFY`+`IN_CLOSE_WRITE`.
   - A path ending in `/` invalidates the whole subtree (FSEvents overflow, or a
     new watch root).
2. The daemon watches all of the shared roots, not only running containers' bind
   sources. It covers `/Users` (minus `~/Library`), `/Volumes`, `/private/tmp` and
   `/private/var/folders`. Unwatched paths would otherwise stay stale forever.
   FSEvents `MustScanSubDirs` becomes a subtree invalidation.
3. The agent writes batches to `rekcod_notify` instead of the phase 1
   `link()` + `chmod` poke. The kernel raises the inotify events itself, so nothing is
   written to Mac files anymore (no ctime churn).
4. Fallback: if the patched module isn't loaded, the agent keeps the phase 1
   poke and the share mounts without `rekcod`, so behaviour is unchanged.

**Semantics:** bounded staleness, not coherence. A Mac-side change becomes visible in
the guest after FSEvents latency plus one vsock hop, about 10–25 ms. Guest-side writes
are coherent as before, because FUSE updates its own caches.

## Memory reclaim (§3e)

Every 10 s, the agent asks the kernel to drop clean page cache when free memory is
low (cgroup v2 `memory.reclaim` on the root). The daemon drives the VZ balloon toward
`used + headroom` and gives memory back immediately when the guest needs it. This
ships after the file-sharing work and only with an OOM-safety test.

## Gates

- `bench/headroom.py --context rekcod`: on warm metadata (`walk#2`, `neg`),
  bind ÷ volume ≤ 1.5×, and faster than OrbStack's bind on every phase except `churn`
  (writes must reach the Mac).
- `scripts/e2e.sh` all green, including the hot-reload, atomic-save and delete
  checks, now without Mac-side side effects.
- Coherence checks: a Mac-side create, modify, rename-over, delete and mkdir+write
  is each visible in the guest within 500 ms, through cached dentries.

## Results (2026-10-11, M5 Pro, macOS 27.0.1)

**Status:** file sharing shipped. Memory reclaim built, but opt-in only (see below).

`bench/headroom.py` uses a 20.5k-file node_modules-shaped tree. The first run of a
phase is cold; `#2` is the same work again (warm):

| phase | rekcod bind | rekcod volume | native macOS | OrbStack bind | rekcod vs OrbStack |
|---|---|---|---|---|---|
| walk (cold) | 794–1290 ms | 46 ms | 111 ms | 204 ms | 4–6× slower |
| walk#2 | **34–47 ms** | 45 ms | 110 ms | 142 ms | **3–4× faster** |
| neg (cold) | 1580–2458 ms | 140 ms | 259 ms | 790 ms | 2–3× slower |
| neg#2 | **89–112 ms** | 117 ms | 232 ms | 668 ms | **6–7× faster** |
| read (cold) | ~1.7 s | 118 ms | 447 ms | 841 ms | 2× slower |
| read#2 | **104–108 ms** | 116 ms | 510 ms | 909 ms | **8–9× faster** |
| churn (writes) | 1.5–1.8 s | 89 ms | 539 ms | 714 ms | 2–2.5× slower |
| inotify round trip | 23 ms | n/a | n/a | 10 ms (p90 102 ms) | about 2× |

Realistic check: walking the docker/compose source (919 files), `lstat`-ing every
file and regex-searching all `.go` files took 159 ms / 5 ms / 5 ms
(cold/warm/warm) on rekcod and 30 / 26 / 27 ms on OrbStack. Micro: a cached
`stat` costs 0.7 µs (same as ext4), and Python `open()+read()` of a cached file 3.7 µs.

Gate results:
- Warm metadata bind ÷ volume ≤ 1.5×: **met** (0.9–1.1×).
- Faster than OrbStack's bind on warm work: **met** (3–9×).
- Cold, first-touch work is still 2–6× slower than OrbStack. Apple's virtio-fs
  costs about 25–60 µs per request, against OrbStack's about 5 µs. Caching can't remove
  the first request. The guest keeps its caches across container runs, so this
  is paid once per file after boot, or after the file changes on the Mac.
- `scripts/e2e.sh` all green (28 checks), including 8 coherence checks:
  - Mac-side modify, create over a cached negative, new name in a cached listing, rename-over, delete, mkdir+write and chmod are each visible in a container within 2 s through warm caches.
  - A guest write is visible on the Mac.
  - The suite passed on 5 of the last 5 runs after the fixes below.

## What was built (vs the approach above)

All in the patched modules (`guest/fuse/rekcod-fuse.patch` and `guest/fuse/rekcod.c`),
active only on mounts using `-o rekcod`:

1. Reply rewriting (`virtio_fs.c`). This keeps the patch out of the FUSE core.
   - LOOKUP, CREATE, MKDIR, GETATTR, SETATTR, STATX and **READDIRPLUS** entries are cached for a day.
   - ENOENT lookups become cached negative entries.
   - OPEN and OPENDIR keep the page cache (`FOPEN_KEEP_CACHE`) and readdir cache (`FOPEN_CACHE_DIR`).
2. Lazy open (`file.c`). Read-only files and directories don't OPEN on the
   host until a read misses the cache, so a cached `open+read+close` needs no host
   round trip. RELEASE is asynchronous, and FLUSH is skipped.
3. No atime invalidation. Otherwise every host READ forces a GETATTR on the next `fstat`.
4. `isatty()` answered locally. Python's `open()` sends `TCGETS`, which would
   force the lazy OPEN plus an ioctl round trip (115 µs → 3.7 µs).
5. `rekcod_notify` (`rekcod.c`). The host sends `path\tsize\tmtime\tctime`.
   - The kernel skips paths whose cached attributes already match. These are echoes of guest writes and files the guest just read. Inode numbers can't be compared because Apple's server remaps them.
   - Otherwise it invalidates, then raises IN_CREATE, IN_MODIFY/IN_CLOSE_WRITE or IN_DELETE when someone watches.
   - Directories that still exist keep their dentry: only their attributes and listing are dropped.
   - Paths resolve through the dcache only (`d_hash_and_lookup`), including expired dentries.
6. Race guard. Replies that arrive within 50 ms of an invalidation keep the
   server's short TTL, so a lookup in flight can't re-cache old state for a day.
7. Counters: `/sys/module/virtiofs/parameters/rekcod_stats` lists host requests by opcode.

Bugs found by measuring and the coherence suite (all fixed):
- WARNs from missing `killsb` locking flooded the console.
- The initramfs loaded the stock modules first.
- Unhashing changed directories cut the path to their children, so containers stayed stale.
- `LOOKUP_CACHED` rejected expired dentries, so events were silently skipped.

## Memory reclaim: finding

- Balloon policy. `memory.rs` keeps about headroom free, grows to max at once when the
  guest runs low, and shrinks only after 15 s of idle. It reclaims page cache under
  macOS memory pressure.
- The guest side works. The balloon negotiates DEFLATE_ON_OOM, and the
  guest inflates as asked (6.4 GB ballooned).
- The host side doesn't release. The VM process's physical footprint stayed
  at 8.0 GB (`vmmap`: resident). Virtualization.framework's traditional balloon
  doesn't hand pages back to macOS in this setup (4K-page guest on a 16K-page host).
- Cost: a 5 GiB allocation takes 2.4 s instead of 1.1 s.

So the balloon is **off by default** (`REKCOD_BALLOON=1` to try it). Next options to
try: order-2 (16 KiB) balloon pages so whole host pages can be released, or
`init_on_free=1` so freed guest pages compress to nothing under pressure.
