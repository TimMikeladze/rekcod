# rekcod plan

Goal: unmodified `docker` CLI + `docker compose` on macOS (Apple Silicon), with
the same compose files working, faster than OrbStack on what developers actually
feel, open source.

**Core decision: don't reimplement the Docker engine. Run upstream `dockerd`
inside the VM and put all of the Rust work into the VM boundary**: file sharing,
networking, memory, and dev-loop features. That's the only layer where speed
comes from, and parity with real Docker comes free.

---

## 1. Audit of the previous plan

| # | Claim | Verdict | Why |
|---|---|---|---|
| 1 | Docker is slow on Mac because of the VM boundary, not Go | ✅ | Correct. Rewriting dockerd in Rust gains nothing. |
| 2 | Write our own Engine API: about 35–40 endpoints, 10–15k LOC, "minimum compat for drop-in" | ❌ | Compose alone calls about 45 distinct engine methods. The list missed `ContainerRename` (used on **every** recreate, `pkg/compose/executor_ops.go`), `CopyToContainer` (secrets/configs with `content:`, `develop.watch`), `DistributionInspect` (pull digest resolution), `ContainerAttach`, `ContainerPause`/`Unpause`, and `ContainerTop`. The CLI adds resize, `docker cp` archive and build. The endpoints are the easy part. The hard part is the daemon behind them: embedded DNS for aliases, the healthcheck runner, the restart supervisor, json-file logs, stdcopy framing, event ordering and registry auth. Even the previous plan conceded it **adds zero speed**. A reimplementation can't reach the 100% parity you asked for. |
| 3 | Builds: "proxy BuildKit in the guest, a few weeks" | ⚠️ moot | Compose builds by shelling out to `buildx bake` (`pkg/compose/build_bake.go`), which talks to the engine's embedded BuildKit over `/grpc` + `/session`. Upstream dockerd already ships that, so builds just work. |
| 4 | DAX is a structural edge, because Virtualization.framework (VZ) lacks it | ⚠️ partly | VZ has no DAX, that's true. libkrun does support DAX on macOS (`krun_add_virtiofs3` shm window), but it's fragile: Hypervisor.framework rejects read-only file mappings, so read-only host files can't be DAX-mapped at all (libkrun fork PR #154). A file truncated on the Mac while the guest has it mapped faults inside the VMM. The win is RAM (no double caching), not metadata speed; the previous plan admits that too. |
| 5 | The bottleneck is metadata (stat/open/readdir storms), not bulk data | ✅ | OrbStack's own blog frames its work as reducing per-call overhead. `bench/headroom.py` measures this directly. |
| 6 | Cache forever in the guest, invalidate exactly from FSEvents, fence Mac-side writes | ⚠️ idea good, mechanism wrong | (a) Upstream Linux virtio-fs has **no host→guest notification queue**. I checked master: `include/uapi/linux/virtio_fs.h` has no `VIRTIO_FS_F_NOTIFICATION`, and the 2019 and 2021 RFCs never merged. We'd need our own guest kernel patch. (b) Mac-side writes can't be fenced: they never pass through the VMM, and hooking them needs the restricted Endpoint Security entitlement. FSEvents is async and coalesced, and it can overflow (`MustScanSubDirs`). The real guarantee is **bounded staleness** (milliseconds), not coherence. That's fine for dev work, but it has to be designed and documented as such. (c) This needs **no custom VMM**: it's a guest kernel patch plus a vsock side channel, so it works on VZ. (d) OrbStack already advertises "custom dynamic caching" on a custom kernel, so this is likely parity work, not a gap. Measure before betting on it. |
| 7 | Injecting inotify beats polling watchers | ❌ baseline | OrbStack and Docker Desktop already deliver Mac file changes to guest inotify, so the baseline isn't polling. **Measured on OrbStack: p50 12.4 ms round trip.** |
| 8 | A 16K-page guest kernel means fewer TLB misses | ❌ breaks drop-in | Prebuilt arm64 images with jemalloc built for 4K pages abort with `Unsupported system page size` (Fluent Bit's arm64 image on Asahi is a known case). Keep 4K pages. The DAX alignment argument is moot because the windows are 2 MB-granular. |
| 9 | vCPU QoS and tickless WFI | ✅ minor | Real, but only possible with your own VMM; VZ hides the vCPU threads. A small battery win, not a pitch. |
| 10 | Restore a warmed stack in under a second (CRIU + VM memory snapshot) | ❌ as framed | A VM snapshot covers **every** project in the VM. On restore you have to handle duplicated RNG state (needs vmgenid), clock jumps, dead TCP connections, and Mac files that changed since the snapshot. CRIU is fragile on real servers. It's also not unique: VZ has had save/restore since macOS 14, and virtio-fs session state has historically blocked restore (libvirt, RHEL-50542). The expensive part of "stack ready" is **migrate + seed**, not process boot. See §3, data snapshots. |
| 11 | Lazy image pulls | ❌ for typical users | Lazy pulls need eStargz, SOCI or nydus-converted images. Docker Hub images are plain gzip layers. |
| 12 | Freeze idle containers | ❌ mostly | Idle containers already use about 0 CPU. Frozen containers break healthchecks and TCP keepalives, and freezing doesn't release memory. |
| 13 | Native networking (routable IPs, `*.project.local`) as an innovation | ❌ | OrbStack already ships container IPs and `*.orb.local`. It's table stakes. |
| 14 | Open source is the biggest lever | ⚠️ | Colima, Lima, Podman Desktop and Rancher Desktop are already open source and free. Open source alone doesn't win; open source **at OrbStack speed** does. |
| 15 | Kill gate: "2× faster than OrbStack on `require('next')`" | ❌ | OrbStack reports 75–95% of native macOS on installs. The ceiling for any bind mount is about VM-local volume speed. The gate must be **headroom** (bind ÷ volume on OrbStack), measured before writing any code. **Measured: about 4× on warm metadata, so 2× on stat-heavy work is reachable. Installs (writes) can't use that headroom.** |
| — | **Missing: Rosetta** | ❗ | Rosetta for Linux works only under VZ. Docker's docs say Docker VMM (their custom HVF VMM) **doesn't support Rosetta, so amd64 emulation is slow**. A libkrun-based rekcod would run every amd64 image under QEMU while OrbStack uses Rosetta, which loses on day one for anyone with an x86-only image. Watch item: Apple calls macOS 27 the last release with general-purpose Rosetta, and has said nothing yet about Rosetta in Linux VMs. |
| — | **Missing: VPN and host networking** | ❗ | Corporate VPN compatibility, localhost port publishing, `host.docker.internal` and IPv6 are where users actually churn. These are parity requirements. |

## 2. Architecture

```
macOS host                                   Linux guest (VZ VM)
┌──────────────────────────────┐   vsock    ┌─────────────────────────────────┐
│ rekcodd (Rust)               │◄──────────►│ rekcod-agent (Rust)             │
│  • VM lifecycle (objc2-virt.)│            │  • applies FS invalidations     │
│  • docker.sock proxy ────────┼────────────┼─► upstream dockerd + containerd │
│    (+ create-hook, §3d)      │            │    + BuildKit (unmodified)      │
│  • port forwarder (/events)  │            │  • memory policy (memory.reclaim│
│  • FSEvents → invalidations  │            │    + balloon)                   │
│ rekcod CLI (snap, import, …) │            │  • volume snapshots (btrfs)     │
└──────────────────────────────┘            │ kernel: 4K pages, Rosetta binfmt│
       /Users ── virtio-fs ────────────────►│  + FUSE "event-coherent" patch  │
                                            └─────────────────────────────────┘
```

- **VMM: Virtualization.framework by default**, for Rosetta, Apple-maintained virtio-fs, net and balloon, and save/restore. Keep a `Vmm` trait so a libkrun backend is possible later (§4, phase 4).
- **Same-path mounts**: `/Users` is mounted at `/Users` in the guest, so bind paths need no rewriting in the proxy.
- **Socket**: create a `rekcod` docker context automatically, and optionally symlink `/var/run/docker.sock` through a privileged helper. Users keep their own CLI and Compose.
- **Licensing**: moby, containerd and BuildKit are Apache-2.0 and fine to ship in the guest image.

## 3. Where rekcod actually wins (ranked)

a. **Data snapshots and branches** for compose volumes. `rekcod snap save seeded` pauses the project's containers, reflink-copies the volume dirs on btrfs (crash-consistent, so Postgres and MySQL recover via WAL), then unpauses. `rekcod snap restore seeded` swaps the dirs back. Optional git-branch binding gives each branch its own DB state. This attacks the real cost (migrate + seed), works on stock dockerd, and nobody does it smoothly today.

b. **`rekcod run <cmd>`: containerize any command with no Dockerfile.** Examples: `rekcod run bun dev`, `rekcod run node script.js`, `rekcod run ./deploy.sh`.
- **Detect**: read the runtime and version from `.bun-version`/`bun.lock`, `.nvmrc`/`engines`, `.python-version`/`pyproject.toml`, `go.mod`, `.tool-versions`/`mise.toml`, or the shebang. Map that to an official image (`oven/bun:<v>`, `node:<v>`, `python:<v>`), with a mise-based image as the fallback for mixed runtimes.
- **Run**: the cwd is bind-mounted at the same path. TTY, signals, exit code and `.env` pass through. Nothing else from `$HOME` is visible.
- **Linux deps**: these live in a VM-local volume keyed by project + lockfile hash (the §3d hook). This is required, not an optimization: Mac-built native modules (esbuild, sharp, swc) crash on Linux. The Mac keeps its own `node_modules` for the editor.
- **Ports**: the agent sees listeners in the container's network namespace and forwards each one to the same port on Mac `localhost`. Reaching a Mac service (say `localhost:5432`) from inside is opt-in via `--host-port 5432`.
- **`--sandbox`**: the command runs in a separate lightweight VM that shares only the project dir. The rest of `$HOME` is invisible, secrets come in only through `--secret`, and an egress allowlist is optional. It needs a separate VM because the shared VM mounts all of `/Users`, so a container escape there reaches all of `$HOME`. Per an Apple forum answer, VZ can swap a running VM's share, which lets one VM be pooled per sandbox; verify this. The pitch is `rekcod run --sandbox npm install` against install-hook worms: Shai-Hulud (Sept 2025) harvested cloud, GitHub and npm tokens from dev machines, and later waves hit hundreds more packages. AI agents running untrusted code are the same use case.
- **`rekcod build`**: the same detection produces an OCI image via Railpack's BuildKit frontend (the actively developed successor to Nixpacks). It runs on stock dockerd through buildx, which is another payoff of not reimplementing the engine.
- **Limits**: "anything" means anything that runs on Linux. Xcode/iOS builds, Metal/GPU, AppleScript and Mac GUI apps are out. The dev loop stays slower than native until §3c lands, so the pitch is isolation + Linux parity + reproducibility, not speed.
- **Gap it fills**: OrbStack's `orb` runs commands in a persistent Linux machine with no isolation and no toolchain detection. Devcontainers need config. devbox/Flox provide toolchains without isolation. `docker init` writes files but doesn't run anything. Railpack and buildpacks only build. Nothing does zero-config + run + isolate in one command.

c. **Event-coherent bind mounts**. A guest FUSE patch adds a mount mode with unbounded entry and attribute TTLs plus a path-based invalidation hook. The agent drives it from host FSEvents over vsock. On overflow it invalidates the whole subtree. It has to handle APFS case-insensitivity: an event for `Foo` must also drop a cached negative entry for `foo`. Phase 0 measured about 4× headroom on warm metadata against OrbStack (see the results section), so this goes ahead.

d. **VM-local directories via labels, with no compose fork**. A service sets `labels: { dev.rekcod.local: "target,.next" }`, and the proxy rewrites `ContainerCreate` to overlay those subpaths with named volumes. Compose forwards labels but drops `x-` extensions, so a label is the hook that works. Caveat: Mac-side editors don't see those dirs. That's safe for build outputs and risky for `node_modules` (the TypeScript language server breaks), so it stays opt-in.

e. **Memory**: the agent proactively reclaims through cgroup v2 `memory.reclaim` (Linux 5.19+), then inflates the balloon to return the freed pages to macOS. VZ only has the traditional balloon device, so the guest has to drive reclaim itself.

f. **Open source plus a scriptable `rekcod` API** for snapshots, status and events. This is the adoption lever, but only on top of speed parity.

## 4. Phases and gates

Each phase ends with a measurement. If a gate fails, the next phase changes.

**Phase 0: measure headroom (no product code).**
- Run `python3 bench/headroom.py --context orbstack --out results.jsonl` and the same with `--context desktop-linux`. Docker Desktop on VirtioFS is the stock VZ baseline, which is what rekcod gets before §3c.
- Also time these on bind and on volume: `pnpm install` (a Next.js app), `tsc --noEmit`, `pytest --collect-only` on a large repo, `git status` on a large repo, incremental `cargo build`, a cold `compose up` until a Postgres + Redis + app stack is healthy, and the VM's idle RSS after a build.
- **Gate:** if OrbStack's warm `walk#2`/`neg` bind ÷ volume is ≤ 1.5×, drop §3c to "parity later" and lead with §3a and §3b. If it's ≥ 3×, §3c is the speed pitch.

**Phase 1: parity skeleton. ✅ Built 2026-10-11 (see `mvp.md`, `README.md`).** VZ VM, a minimal kernel with Rosetta binfmt, upstream dockerd, the socket proxy, the port forwarder, `host.docker.internal`, VPN-safe outbound networking, `rekcod import`, and Mac→container file events.
- Results: all 22 checks in `scripts/e2e.sh` pass. Cold start to dockerd-ready takes 1.3 s, the idle VM uses about 160 MiB RSS, and the inotify round trip is 25 ms.
- **Not yet measured:** Compose's own e2e suite (`docker/compose` `pkg/e2e`). That gate is still open.
- **Gate:** Compose's own e2e suite (`docker/compose` `pkg/e2e`) passes at Docker Desktop's rate, and `bench/headroom.py --context rekcod` runs.

**Phase 2: speed. ✅ §3c shipped, ⚠️ §3e opt-in (2026-10-11, see `phase2.md`).** §3c (if phase 0 allows) and §3e.
- Warm bind-mount work beats OrbStack 3–9×. Cold first-touch is still 2–6× slower.
- The balloon doesn't lower the host footprint on macOS 27, so it stays off by default.
- **Gate:** warm metadata bind ÷ volume ≤ 1.2×. A correctness suite passes: fsx-style random ops, rename and unlink races, FSEvents overflow, case-aliasing, and Postgres on a bind mount. Idle RSS is at or below OrbStack's.

**Re-sequenced 2026-10-11 (adoption first, see `handoff.md`):** 2b closes the open phase-2 gates. 3 ships it (releases, guest auto-fetch, signing and notarization, brew, CI). 4 works through the parity backlog. 5 is the differentiators below. 6 is the VMM and file-server research below.

**Phase 5 (was 3): differentiators.** §3a and §3b, then §3d and §3f. §3b needs only phase 1 plus the §3d hook, so it can start right after phase 1, in parallel with phase 2.
- **Gate:** restoring a seeded Postgres snapshot and reaching healthy is faster than `up` + migrate + seed by an order of magnitude.
- **Gate (`rekcod run`):** a warm `rekcod run node -v` adds ≤ 300 ms over native. `rekcod run <dev cmd>` serves on Mac `localhost` with hot reload, with zero config, on the Next.js, Vite, Bun/Hono, Express, FastAPI and Django starters. Under `--sandbox`, a test install hook can't read `~/.ssh` or reach a non-allowlisted host.

**Phase 6 (was 4, optional): libkrun backend** for DAX and memory experiments. Promote it only if the RAM win is large on a monorepo build **and** Rosetta isn't needed (Apple ends Linux Rosetta, or the target users run only arm64 images).

## 5. Non-goals

Reimplementing the Engine API, Compose or BuildKit. 16K-page kernels. Lazy pulls. Per-container microVMs as the default (only `rekcod run --sandbox` gets its own VM). Windows and Linux hosts. CRIU process checkpoints.

## 6. Top risks

1. The guest FUSE patch has to be carried across kernel upgrades. Keep it small and behind a mount option.
2. Rosetta for Linux VMs after macOS 27 is unannounced. If it goes away, the VZ vs libkrun trade flips; revisit phase 4.
3. Parity surface (networking, VPN, the menu bar app) has to be close to OrbStack before any differentiator matters.
4. FSEvents gives bounded staleness, not coherence. Some tool will notice. Provide a per-mount `strict` mode that falls back to short TTLs.
5. `--sandbox` is a security claim: a share-scoping bug becomes a credential leak. Write a threat model and get an external review before marketing it as security.

## Phase 0 results (M5 Pro, 48 GB, macOS 27.0.1, OrbStack 2.2.3, engine 29.4.0)

`python3 bench/headroom.py`, 20.5k-file node_modules-shaped tree. Runs 2 and 3 of 3, which agreed within ~10%. Run 1 had a warm host cache.

| phase | native APFS | OrbStack volume | OrbStack bind | bind ÷ volume | bind ÷ native |
|---|---|---|---|---|---|
| walk (cold) | 106 ms | 33 ms | 279 ms | 8.6× | 2.6× |
| walk#2 (warm) | 102 ms | 32 ms | 136 ms | **4.2–4.3×** | 1.3× |
| neg (missing-file lookups) | 220–308 ms | 103 ms | 465 ms | **4.5×** | 1.5–2.1× |
| read | 444–533 ms | 82 ms | 622 ms | 7.4–7.9× | 1.2–1.4× |
| churn (create + delete) | 560–635 ms | 65 ms | 606–847 ms | 9–13× | ~1–1.5× |

Inotify round trip on a bind mount (Mac write → guest event → guest writes a reply → Mac sees it): **p50 12.4 ms, p90 13.4 ms**. Edits about 50 ms apart are coalesced and arrive in about 45 ms. OrbStack also delivers each event twice.

What this means:
- **Gate passed (≥ 3×).** On OrbStack, warm metadata on a bind mount is about 4× slower than a VM-local volume, so §3c is the speed pitch. OrbStack caches somewhat (cold → warm is 2×), but warm hits still cross the VM boundary, and missing-file lookups barely benefit at all. Event-coherent caching targets exactly that.
- **The ceiling is above native macOS.** Linux in the VM is 3–6× faster than APFS on this workload. A bind mount served from guest cache would beat running natively on the Mac.
- **Writes (churn) can't be cached away.** They have to reach the Mac. `npm install` and `rm -rf node_modules` speed comes from §3d (VM-local dirs), not §3c.
- **Inotify forwarding already exists** (claim 7 confirmed): about 12 ms with no polling. That's parity work, not a differentiator.
- **Pending:** the Docker Desktop baseline (it fails with `Docker Desktop is unable to start`) and real workloads (`pnpm install`, `tsc`, `pytest --collect-only`, `git status`, compose up → healthy).

**rekcod after phase 1** (stock Apple virtio-fs, no caching work yet; same benchmark):

| phase | rekcod volume | rekcod bind | OrbStack bind | rekcod bind ÷ OrbStack bind |
|---|---|---|---|---|
| walk (cold) | 47 ms | 1046 ms | 279 ms | 3.7× slower |
| walk#2 (warm) | 40 ms | 383 ms | 136 ms | 2.8× slower |
| neg | 125 ms | 2169 ms | 465 ms | 4.7× slower |
| read | 135 ms | 3907 ms | 622 ms | 6.3× slower |
| churn | 88 ms | 2430 ms | ~700 ms | 3.5× slower |
| inotify round trip | n/a | 25 ms | 12.4 ms | 2× slower |

Volumes match OrbStack (40 vs 32 ms warm walk). On bind mounts, stock VZ virtio-fs is where rekcod loses today. Phase 2 (§3c) is the next priority before claiming speed. The same FSEvents channel built in phase 1 can carry the invalidations.

## Sources

- Compose source, `docker/compose` at e11dce5: `pkg/compose/executor_ops.go`, `secrets.go`, `watch.go`, `pull.go`, `build_bake.go`
- [Linux `virtio_fs.h` (master)](https://github.com/torvalds/linux/blob/master/include/uapi/linux/virtio_fs.h), [notification queue RFC](https://lkml.iu.edu/hypermail/linux/kernel/1911.1/09740.html), [LWN: notification queue](https://lwn.net/Articles/871469/), [inotify-over-virtiofs RFC](https://lkml.kernel.org/linux-fsdevel/YYMNPqVnOWD3gNsw@redhat.com/t/)
- [Docker VMM docs (no Rosetta)](https://docs.docker.com/desktop/features/vmm/)
- [libkrun macOS DAX fix, PR #154](https://github.com/superradcompany/libkrun/pull/154)
- [OrbStack: fast filesystem](https://orbstack.dev/blog/fast-filesystem), [OrbStack architecture](https://docs.orbstack.dev/architecture)
- [Asahi broken software (jemalloc 16K)](https://asahilinux.org/docs/Broken-Software), [Fluent Bit arm64 16K issue](https://git.tdem.in/fluent/fluent-bit/issues/8802)
- [Rosetta (Wikipedia)](https://en.wikipedia.org/wiki/Rosetta_(software)), [Rosetta for Linux VMs notes](https://blog.inoki.cc/2026/02/28/Apple-Rosetta-Linux-VM-Secret-en/)
- [Railpack intro](https://blog.railway.com/p/introducing-railpack), [Coolify: Railpack vs Nixpacks](https://coolify.io/docs/applications/build-packs/railpack)
- [Shai-Hulud analysis (Blackpoint)](https://blackpointcyber.com/blog/inside-the-shai-hulud-npm-supply-chain-attack/), [SHA1-Hulud wave (Snyk)](https://snyk.io/de/blog/sha1-hulud-npm-supply-chain-incident), [CSA note](https://labs.cloudsecurityalliance.org/research/csa-research-note-shai-hulud-npm-worm-ai-developer-supply-ch/)
- [Apple forum: change VZ share at runtime](https://developer.apple.com/forums/thread/732128), [Apple container (InfoQ)](https://www.infoq.com/news/2025/06/apple-container-linux)
- [VZ `validateSaveRestoreSupport`](https://developer.apple.com/documentation/virtualization/vzvirtualmachineconfiguration/validatesaverestoresupport()), [RHEL-50542 virtiofs restore](https://issues.redhat.com/browse/RHEL-50542)
