# Handoff prompt

Paste everything below the line into a fresh session started in this repo.

---

You're continuing **rekcod**: Docker on macOS (Apple Silicon) that runs the real, unmodified
`dockerd` in a Virtualization.framework VM. Users switch to it in one command (`rekcod start`
selects a `rekcod` docker context, `rekcod import --from <context>` copies images and volumes)
and back in one (`rekcod stop`). The goal is the fastest adoption, with seamless
switching from Docker Desktop and OrbStack. Repo: github.com/TimMikeladze/rekcod (private),
branch `main`.

## Read first

- `README.md`: what works, commands, build/test, known gaps.
- `docs/plan.md`: strategy, the audit of the original idea, phases and gates. §3 lists the differentiators.
- `docs/mvp.md`: phase 1 design decisions.
- `docs/phase2.md`: the bind-mount caching design, measured results, the bugs found, and the memory finding.
- `guest/fuse/README.md`: how to edit the kernel patch.

## State (2026-10-11)

- **Phase 1 is done.** It has switch-over, import, ports, DNS via the Mac and Rosetta, and boots in 1.3 s.
- **Phase 2 bind mounts are done.** Patched `fuse`/`virtiofs` modules give long TTLs, lazy open, and FSEvents-driven `rekcod_notify` invalidation with stat comparison. Warm work is 3–9× faster than OrbStack; cold first-touch is 2–6× slower.
- **Memory reclaim (balloon) is opt-in** with `REKCOD_BALLOON=1`. On macOS 27 the guest inflates the balloon but the host footprint doesn't drop.
- **Tests:** 18 unit tests (`cargo test --workspace`), clippy clean, and `scripts/e2e.sh` passes 28/28, run 5× in a row.
- **Machine:** M5 Pro, macOS 27.0.1.
  - Docker Desktop is installed but broken (`Docker Desktop is unable to start`).
  - OrbStack is installed and is the comparison baseline (`--context orbstack`).
  - rekcod can rebuild its own guest image (`REKCOD_BUILD_DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock scripts/build.sh`).

## Ground rules (don't relitigate)

1. **Never reimplement the Docker API, Compose or BuildKit.** Parity comes from upstream dockerd (pinned in `guest/Dockerfile`).
2. **Virtualization.framework, not libkrun/HVF**, because only VZ gets Rosetta (Docker VMM lacks it).
3. **Host↔guest traffic is vsock only:** ports 2375 docker, 2376 port forward, 2377 control, 2378 DNS (`crates/rekcod-proto`). No TCP API and no reliance on vmnet routing.
4. **File-sharing semantics are bounded staleness (about 25 ms), not coherence.** Guest writes are coherent. Never write to Mac files to signal the guest.
5. **4K-page guest kernel**, because 16K breaks jemalloc in prebuilt images.
6. **Never kill whatever holds a Mac port.** Report it in `rekcod status` instead.
7. **Spec first** in `docs/` for multi-file work, then build, then verify with real runs. Report measured numbers and say what you didn't verify.

## Hard-won gotchas

- **An unread vsock connection freezes the whole VM.** VZ writes guest data with a blocking `writev` on the vsock device queue that vCPUs wait on. `Vm::connect` returns `vm::Conn`, which always drains.

- **`virtio-rng` must load from the initramfs** (`modules=` on the kernel cmdline). Without it, boot stalls about 5.6 s waiting for entropy.
- **The initramfs autoloads virtiofs**, so the Dockerfile reruns `mkinitfs` after swapping in the patched modules.
- **Apple's virtio-fs server:**
  - It remaps inode numbers, so compare size/mtime/ctime instead.
  - It costs about 25–60 µs per request.
  - It answers READDIRPLUS, so those entries need their TTLs rewritten too.
- **Kernel patch traps** (`rekcod.c`, `virtio_fs.c`):
  - `fuse_reverse_inval_*` need `down_read(&fc->killsb)`.
  - Never `d_invalidate` a directory that still exists.
  - Resolve notify paths with the dcache walk (`rekcod_walk`), not `LOOKUP_CACHED`.
  - Keep the 50 ms race guard.
  - Atime invalidation and `isatty` ioctls each cost a host round trip per file.
- **FSEvents flags are sticky.** The host dedupes by fingerprint and skips `~/Library` and `~/.rekcod`. OrbStack, for comparison, delivers duplicate events.
- **The vmnet gateway refuses DNS**, and Mac resolvers can be link-local IPv6. That's why DNS goes guest dnsmasq → agent stub → vsock → daemon → Mac's `/etc/resolv.conf`.
- **Busybox init children get a minimal PATH.** `run.sh` exports it.
- **Signing:** the binary needs `com.apple.security.virtualization` (`scripts/build.sh` ad-hoc signs it).
- **Debug tools:**
  - `rekcod shell` gives a root shell in the guest.
  - `~/.rekcod/logs/{daemon,console}.log`.
  - `REKCOD_DEBUG=1` logs forwarded file events.
  - `REKCOD_KERNEL_CMDLINE` overrides the guest cmdline.
  - `cat /sys/module/virtiofs/parameters/rekcod_stats` in the guest shows host requests per FUSE opcode (this is how the remaining round trips were found).

## Verify any change

```sh
cargo test --workspace && cargo clippy -p rekcod -p rekcod-proto
scripts/build.sh                                    # needs a docker engine for the guest image
E2E_IMPORT_FROM=orbstack scripts/e2e.sh             # 28 checks; run it 3x for anything touching fuse
python3 bench/headroom.py --context rekcod          # compare: --context orbstack
```

## Next phases (adoption first)

### Phase 2b: done (2026-10-11)

See `docs/phase2b-4.md` (results) and `tests/stress/`. The case-insensitivity bug was real and is fixed.
Also found and fixed a pre-existing whole-VM freeze: VZ blocks a vCPU when a vsock
connection isn't drained (`phase2b-4.md`, "VM freeze"). **Never let a vsock
connection go unread**; always use `vm::Conn`.

### Phase 3: ship it (nobody can install it yet)

1. **Prebuilt artifacts** (rekcod binary, `out/guest/*`, checksums) on GitHub Releases.
   - Shrink `root.img` first: it's 584 MB, so drop unused kernel modules and consider squashfs or erofs plus a writable overlay.
2. **Auto-fetch the guest:** `rekcod start` downloads and verifies the guest on first run into `~/.rekcod/guest`.
3. **Developer ID signing and notarization**, keeping the virtualization entitlement.
   - **Blocked on the user:** needs their Apple Developer account and certs. Ask.
4. **Brew tap** (`TimMikeladze/tap/rekcod`) with a `rekcod upgrade` flow.
5. **CI (GitHub Actions):** unit tests and clippy on macOS arm64; the guest image built on an arm64 Linux runner.
   - Hosted macOS runners can't nest VMs, so e2e needs a self-hosted Mac. Ask before setting one up.

### Phase 4: done except the menu bar item

UDP ports, `host.docker.internal` → Mac loopback, Mac `/tmp`, DNS over TCP, `rekcod compact`
plus hourly trim, and bind-mount warming are shipped and checked in `scripts/e2e.sh`.
`init_on_free=1` was measured and rejected.

### Phase 5: done (2026-10-12, see `docs/phase5.md`)

Snapshots (`rekcod snap`), the zero-config runner (`rekcod x`, with autoports and
`--sandbox`), `dev.rekcod.local` VM-local dirs via the socket proxy, `status --json`,
the menu bar item, warm-at-boot, and ~14 ms change latency. Memory return is still open.

### Phase 5 (original plan): differentiators (`docs/plan.md` §3)

1. **`rekcod run <cmd>`** (§3b): zero-config containerize. Then `--sandbox`, a separate VM sharing only the project dir. This is a security claim, so write a threat model first.
2. **Data snapshots** (§3a): `rekcod snap save/restore`, using a reflink copy of volume dirs while paused, with optional git-branch binding.
3. **Label-driven VM-local dirs** (§3d): `dev.rekcod.local`, done through the socket proxy's `ContainerCreate` rewrite.
4. **Scriptable `rekcod` API** (§3f).

### Phase 6: done, both "no" (`docs/research.md`)

A custom vsock file server is slower per request than Apple's virtio-fs (63 vs 46 µs). libkrun would lose Rosetta.

Phase 3 progress: see `docs/phase3.md` (guest shrink, releases, auto-fetch, CI done; signing/notarization and brew open).

**Start with:** Developer ID signing + notarization (needs the user's Apple account).
