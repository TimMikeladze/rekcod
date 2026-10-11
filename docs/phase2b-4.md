# Phases 2b, 4, 6: correctness gates, switcher gaps, research

Scope: `docs/handoff.md` phases 2b, 4 and 6. Phase 3 (shipping) and 5
(differentiators) are out of scope here.

## Phase 2b: close the phase-2 gates

| gate | approach | test |
|---|---|---|
| case-insensitive APFS | `rekcod_drop_case_aliases()` unhashes every ASCII case variant of a name in its parent: on host notify (`rekcod_one`) and on guest create/mknod/mkdir/symlink/link/unlink/rmdir/rename (`rekcod_entry_changed` in `dir.c`). Covers stale negatives (`foo` after `Foo` is created) and stale positive aliases (`FOO` after `Foo` is deleted). | `scripts/e2e.sh` coherence: 4 case checks |
| FSEvents overflow | already mapped: `need_rescan()` (MustScanSubDirs, user/kernel dropped) becomes a subtree invalidation. Verify under a burst big enough to drop events. | `tests/stress/overflow.sh` |
| fsx-style random ops | Python driver: random create/write/append/truncate/rename/unlink/mkdir on the Mac and in a container; after each op the container's view (content hash per file, listing) must converge to the Mac's within 1 s. | `tests/stress/fsx.py` |
| Postgres on a bind mount | `postgres:17` with `PGDATA` on a Mac dir, `pgbench -i -s 10` + 30 s run, restart the container, `amcheck` + row counts. | `tests/stress/postgres.sh` |
| Compose e2e suite | clone `docker/compose` at the CLI's version, run `pkg/e2e` against rekcod and OrbStack, record pass rates. | `tests/stress/compose-e2e.sh` |

Decision: case folding is ASCII only. APFS folds Unicode too, but non-ASCII
case variants of the same name in one directory are rare, and the cost is only a
cache miss in the guest, never wrong data.

## Phase 4: gaps switchers hit

1. **Cold first-touch warming.** On container start the daemon sends
   `warm <path>` per new bind source (only when the guest caches). The agent
   lists and lstats the tree with 8 low-priority threads, up to 200k entries.
   No file contents are read. (readdir alone was measured not to fill the
   dentry cache through Apple's server, so each name gets its own lstat.)
2. **UDP published ports.** Mac `UdpSocket` per binding. Each Mac client address
   gets one vsock stream to the agent's `FORWARD_PORT` with header `udp:<port>`;
   datagrams are framed `u16 len + payload` both ways. The agent sends from a
   connected socket to `127.0.0.1:<port>` (docker-proxy listens there). Flows idle
   for 60 s are dropped.
3. **`host.docker.internal` reaches Mac `127.0.0.1` services.** Guest iptables
   DNATs TCP to the gateway IP onto the agent (`REDIRECT --to-ports 2379`). The
   agent reads `SO_ORIGINAL_DST` and takes a parked connection from a pool the
   host keeps open on vsock `REVERSE_PORT` (2379), writes `connect <port>`, and
   the host splices it to `127.0.0.1:<port>`. No VZ listener delegate needed.
   Services bound to `0.0.0.0` on the Mac also work through this path.
4. **Mac `/tmp`.** The guest bind-mounts `/private/tmp` over `/tmp` (as OrbStack
   does). The watcher maps `/tmp/...` and `/var/folders/...` bind sources to
   their `/private/...` Mac paths.
5. **DNS over TCP.** The host retries over TCP when an answer has TC set. The agent
   serves TCP on `127.0.0.1:5353` too; for UDP clients it truncates answers over
   4096 bytes (header + question, TC=1) so dnsmasq retries over TCP.
6. **Data disk compaction.** Check that VZ's virtio-blk advertises discard; if so
   the agent runs `fstrim /var/lib/docker` hourly and on `rekcod compact`.
7. **Memory.** Measure `init_on_free=1` with `footprint` of the VZ XPC process;
   enable by default only if it lowers the footprint.
   Order-2 balloon pages: not attempted (the balloon itself doesn't release on
   macOS 27, see `phase2.md`).
8. Menu bar item: not attempted (optional).

## Phase 6: research

- **Cold metadata latency.** Prototype: the host serves `lstat`/`readdir` over a
  vsock connection (`META_PORT` 2380); `rekcod-agent bench-meta <dir>` measures
  per-request latency against the same requests through virtio-fs (cold, after
  `drop_caches` + subtree invalidation). Decision rule: a custom server is worth
  building only if it is ≥3× faster per request.
- **Memory release via libkrun.** Desk research only: what it would gain
  (free-page reporting) and lose (Rosetta). Written up in `docs/research.md`.

## Results (2026-10-11, M5 Pro, macOS 27.0.1)

**Phase 2b**
- Case-insensitivity: bug reproduced (host and guest creates left `foo` as
  ENOENT after `Foo` was created; deleted `Qux` stayed visible as `qux`) and fixed.
  `scripts/e2e.sh`: 37/37, including 4 new case checks.
- fsx: 1,800 random ops over 4 seeds, all converged; slowest 213 ms including one
  `docker exec` (~40 ms).
- FSEvents overflow: a real 200k-file burst (12 s) did not make FSEvents drop
  events, and nothing went stale. With the test hook forcing the drop path:
  172 subtree invalidations, 0 stale entries out of 201 cached ones.
- Postgres on a bind mount: all checks pass (init, pgbench, clean restart,
  `kill -9` recovery, amcheck). `pgbench -c8 -T30`: 5,697 tps vs OrbStack 6,766;
  `pgbench -i -s 10` 4 s vs 1 s.
- Compose e2e: see "Compose suite" below.

**Phase 4**
| item | result |
|---|---|
| warming | `find` over 20k files 3 s after container start: 0.89 s cold → 0.14 s warmed (6×). Warm walk takes 0.5–1.1 s in the background. |
| UDP ports | `-p 15353:5353/udp` echo round trip from the Mac works |
| host loopback | `wget host.docker.internal:18777` reaches a Mac server bound to 127.0.0.1 |
| Mac `/tmp` | `-v /tmp/x:/f` shows the Mac file |
| DNS over TCP | `dig +tcp TXT google.com` via guest dnsmasq: 19 records; >4 KB UDP answers come back TC |
| compaction | 2 GiB written and deleted in a volume: `data.img` 25.3 → 23.1 GiB on the Mac after trim. `rekcod compact` + hourly trim. |
| `init_on_free=1` | **rejected**: VM footprint 8,239 MB right after boot (the kernel zeroes all RAM) vs 644 MB default; default is 751 MB after allocating and freeing 3 GB |

**VM freeze (found by the Compose suite, fixed).** The whole guest froze 3–20
min into docker/compose's e2e suite, on the last commit too (pre-existing).
- Symptom: every vCPU in `Hv::Vcpu::run → wait_for_interrupt` except one; guest
  soft lockups waiting on IPIs (`kick_all_cpus_sync`, `smp_call_function_single`).
- Cause (sampled in the VZ process): the vCPU that never answered was blocked in
  `dispatch_sync` on VZ's `vsock-device` queue, which was itself blocked in a
  **blocking `writev`** to the daemon's end of a vsock connection whose buffer
  was full. The daemon stops reading a connection whenever the Mac-side client
  stops reading (`copy_bidirectional` backpressure), e.g. `docker logs -f` piped
  into something slow. One stalled client froze the whole VM.
- Fix: `vm::Conn` reads every vsock connection eagerly into memory; a reader more
  than 64 MiB behind gets EOF while the data keeps being drained and discarded.
- Regression test: `tests/stress/slow-reader.sh` (old daemon: engine dead after
  5 s; fixed: responsive for 30 s, daemon RSS 48 MB).
- Ruled out along the way: guest kernel (6.18.55 stock froze too), the fuse patch,
  the balloon device, vCPU count, `nohz=off`. The Dockerfile keeps the
  `KERNEL_REPO`/`FUSE_PATCH` build args and the daemon `REKCOD_NO_BALLOON` knob for
  future bisects. OrbStack had the same symptom class (orbstack#2471, fixed in 2.2.0).

**Compose suite (docker/compose v5.4.0 `pkg/e2e`, 163 top-level tests):** **rekcod
153 passed / 10 failed, OrbStack 155 / 8** (clean engines, same Mac). 8 failures are
shared by both. rekcod-only:
- `TestBuildPlatformsStandardErrors`: expects the classic image store to reject
  multi-platform builds. Upstream dockerd 29 defaults to the containerd store,
  which supports them. Expected; not a rekcod bug.
- `TestPreStartHookInError`: flaky; it passed on an isolated rerun.

(OrbStack-only failure: `TestLocalComposeVolume`, which also failed on rekcod.)
An earlier run left behind networks from killed runs, which exhausted Docker's
default address pools. Prune between runs.
