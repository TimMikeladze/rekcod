# Efficiency plan

Changes around upstream dockerd that make rekcod cheaper to run on a Mac: less memory,
fewer copies, fewer round trips. dockerd, BuildKit and Compose stay unmodified. Each item
says where the code is today and what number decides whether it ships. Done so far
(2026-11-08): the kernel move to 7.3 and current pins. The rest isn't built yet.

Old measurements quoted below come from the docs removed in c12de52; read them with
`git show bdfa761:docs/perf-plan.md` (also `research.md`, `platform.md`, `handoff.md`).

## Benchmarks to write first

Existing: `bench/cold_start.sh`, `postgres.sh`, `tcp_rtt.py`, `inotify_rtt.py`,
`headroom.py`, `sandbox.sh`. The gates below need these too:

- `bench/idle.sh`: CPU time and wakeups of the `com.apple.Virtualization.VirtualMachine`
  process over 60 s, with no containers and with a Compose stack up.
- `bench/pull.sh`: cold `docker pull` of a ~1 GB image, in the main VM and in a sandbox.
- `bench/sandbox_net.sh`: download throughput inside a sandbox from a host it may reach
  (`--reach`).
- `bench/amd64.sh`: start time of an amd64 image, and an amd64 `npm ci`.
- `bench/diff.sh`: `sandbox diff` on a large repository with one changed file.
- `bench/snap.sh`: `snap save` and `snap restore` time against volume size.
- Fleet timings in `scripts/e2e-fleet.sh`: deploy, scale from 1 to 2, wake after
  `idle_stop`, idle footprint per instance.

Run them on an isolated VM (`REKCOD_HOME` with a short path), never the VM running your
containers. Record the baselines in this file.

## Local engine configuration

These all live in the `daemon.json` that `boot.sh` writes, so they ship in one guest
release.

### Merge a user daemon.json

`boot.sh:184` writes `/etc/docker/daemon.json` from scratch on every boot. There's no way
to set `insecure-registries`, `registry-mirrors`, `default-address-pools`, `dns` or
`features`, which Docker Desktop users set in its settings. Read `~/.rekcod/daemon.json`
through the Mac share and merge it over the generated file, with rekcod's own keys
(`host-gateway-ips`, `shutdown-timeout`) winning. Do the merge in a `rekcod-agent`
subcommand that `boot.sh` runs, not in shell. A file that doesn't parse is skipped, and
`rekcod doctor` reports it.

Gate: an e2e check that an `insecure-registries` entry shows up in `docker info`.

### Rotate container logs by default

There are no `log-opts`, so json-file logs grow until the data disk is full. Default to
`"log-opts": {"max-size": "10m", "max-file": "3"}`. It applies to containers created
after the change, and the user's daemon.json can override it.

### Collect build cache by free space

`boot.sh:182` sets `defaultMaxUsedSpace: 20GB` whatever `--disk` is. Add
`defaultMinFreeSpace` so BuildKit drops cache before the data disk fills.

### Raise file descriptor limits (verify first)

dockerd starts from busybox init through `run.sh` with no `ulimit`, so I suspect
containers get a soft limit of 1024 and a hard limit of 4096. Docker Desktop containers
get 1048576. Check with:

```sh
docker run --rm alpine sh -c 'ulimit -Sn; ulimit -Hn'
```

If it's low, raise it in `run.sh` before dockerd starts.

### Download more layers at once

dockerd downloads 3 layers at a time (`max-concurrent-downloads`). Try 6 and 10.

Gate: `bench/pull.sh` at least 15% faster, or keep the default.

### Keep containers through a dockerd crash

init respawns dockerd when it exits, and its containers stop with it. `live-restore: true`
keeps them running. It doesn't help across a VM restart, so it only matters for dockerd
crashes and OOM kills.

## Local engine: rekcod's own code paths

### Cache Rosetta translations

`vm.rs:336` creates the Rosetta share without `cachingOptions`. Set
`VZLinuxRosettaUnixSocketCachingOptions` and start `rosettad` from `boot.sh` with its cache
on the data disk, so amd64 binaries are translated once rather than on every run. If
objc2-virtualization doesn't bind the class, reach it by name the way `elastic.rs` does.

Gate: `bench/amd64.sh`.

### Scan less for autoports

Every second, for each `rekcod x` container, the agent reads every `/proc/*/cgroup` and
every fd of the matching processes (`agent.rs:387`). A container with its own network
namespace owns every listening socket in it, so one process's `/proc/<pid>/net/tcp{,6}`
is enough, with the pid taken from the container's `cgroup.procs`. Keep the fd walk for
host-network containers.

Gate: agent CPU in `bench/idle.sh` with a `rekcod x` dev server running.

### Forward host-network containers' ports

Docker Desktop forwards ports that `--network host` containers listen on to the Mac.
rekcod does this only for containers labelled `dev.rekcod.autoports` (`ports.rs:35`).
Treat host-network containers as autoport containers. This changes behavior, so it gets a
README note.

### Replace docker-proxy with an agent stub (measure first)

The agent already dials containers directly and only reads docker-proxy's command line
(`proxyport.rs`). dockerd still runs one docker-proxy per published port and address
family. `userland-proxy-path` could point at a `rekcod-agent` mode that holds the port,
reports ready and relays guest-local connections.

Gate: total docker-proxy RSS for a typical Compose stack. Under 50 MB, drop it.

### Fewer idle wakeups (measure first)

Periodic work today: the daemon's 500 ms state probe (`daemon.rs:305`), the 30 s clock
sync, 1 s autoports polling and the hourly trim. Change only what `bench/idle.sh` shows.

### Narrow the Mac file watch (measure first)

The watch covers `/Users` except `~/Library` and `~/.rekcod` (`watch.rs:97`), so a
Mac-side `cargo build` in a repo no container mounts still gets stat'ed and forwarded. If
daemon CPU during that build is noticeable, watch only the remembered bind roots
(`~/.rekcod/vm/warm-paths`) and invalidate a subtree when a new root appears, as `/private/tmp`
binds already do.

## Storage

### Checkpoint the whole engine

Freeze `/var/lib/docker` (`FIFREEZE` from the agent), `clonefile` `data.img`, unfreeze.
APFS clones copy metadata only, so I expect milliseconds at any disk size. Restoring swaps
the disk and boots (1.3 s). It covers every project at once, so it's a new command rather
than a `snap` mode, and it can also take an automatic rollback point before a guest
upgrade. A new command needs a `Kind` in `actions.rs` and a README section.

### Reflink volumes for snap

`snap save` and `snap restore` copy with `cp -a` (`snap.rs:351`), so time grows with the
data. With XFS (reflink) under `/var/lib/docker/volumes`, copies are metadata only, so
time follows the number of files rather than their size.

Put volumes on their own disk, `vm/volumes.img` (`/dev/vdc`, XFS), mounted at
`/var/lib/docker/volumes`. Images and containers stay on the ext4 data disk, untouched.
Needs `CONFIG_XFS_FS=y`, `xfsprogs` in the rootfs and a reflink copy (`cp --reflink` or
`FICLONE`) in place of the helper's `cp -a`.

Existing installs migrate once, on the first boot of the guest that adds the disk:

1. Copy `/var/lib/docker/volumes` (volume data and `metadata.db`) from the ext4 disk to the
   new disk before dockerd starts.
2. Mount the new disk at `/var/lib/docker/volumes` only after the copy finishes. A copy
   interrupted by a crash or power-off starts over on the next boot; the originals are
   never touched while it runs.
3. Keep the originals until the next `rekcod compact`, which removes them.

The copy takes time in proportion to the volume data, and `rekcod start` shows progress
for it. Two other options were rejected: XFS only for new installs leaves existing users
with slow snapshots and two data-disk formats to support forever; converting the whole data
disk to XFS copies every image and container too.

A separate disk also allows a volumes-only checkpoint: `clonefile` `volumes.img`.

Gate: `bench/snap.sh` with a 5 GB volume.

### Batch image import

`rekcod import` runs one `docker save` per image (`cli.rs:871`), so a base layer shared by
ten images is copied ten times. One `docker save a b c` writes it once. Fall back to one
image at a time on error, keeping the platform retry.

### Disk device and ext4 options

The data disk is virtio-blk with one queue (`vm.rs:277`). Try
`VZNVMExpressControllerDeviceConfiguration`, and `lazytime` plus `commit=` on the data
disk mount. This is left over from the old perf plan.

Gate: 10% on fio 4k randwrite with fsync, with `REKCOD_DISK_SYNC` semantics unchanged.

### containerd image store size

I believe Docker 29 uses the containerd image store on fresh installs, which keeps
compressed layers next to the unpacked snapshots. Compare `docker system df` with the
allocated size of `data.img`. If the overhead is large, document turning it off through
the user daemon.json. The default stays.

## Images

### One pull cache per Mac

The main VM, every sandbox VM, `x --sandbox`, golden disk builds and fleet nodes all pull
from the internet separately, and each counts against Docker Hub's anonymous pull limit.
The daemon would serve a pull-through registry mirror over vsock, with blobs under
`~/.rekcod/blobs`. `registry-mirrors` only covers Docker Hub. For ghcr.io and the rest,
containerd needs `hosts.toml`, and I don't know whether dockerd 29's containerd image
store reads it. Spike that before anything else here.

Gate: a second cold pull of the same image in a new sandbox at least 3× faster.

## VM, kernel and CPU

### Bring vCPUs online on demand

Chatty workloads lost about 20% going from 4 to 6 vCPUs (old perf plan). The agent would
take vCPUs offline while the run queue is short and bring them back when it grows. First
experiment: offline 2 of 6 and rerun pgbench. If it doesn't match the 4-vCPU numbers, drop
it.

### Run the newest stable kernel (done)

The guest runs the newest stable series on its newest point release: 7.3.3 since
2026-11-08, up from 6.12.115. It moves to a new series once that series has its `.2` point
release, or right away when the current one is end of life. Each move rebases the fuse
patch (`guest/fuse/`) and runs `scripts/e2e.sh` three times.

What the move to 7.3 needed:

- Fuse patch: three-way rebase from 6.12 onto 7.3's `fs/fuse`. Poll moved to `poll.c`,
  `fuse_release_end` lost its `fm` argument, `create_new_entry` returns a dentry, and
  `d_hash_and_lookup` is no longer exported (`try_lookup_noperm` replaces it). Since 6.16 a
  READDIRPLUS reply can span several pages, so the TTL rewrite in `virtio_fs.c` now copies
  each record header across page boundaries instead of walking one page at a time.
- Elastic memory driver: built unchanged.
- Kernel config: vendored as `guest/kernel/base.config` (Alpine edge's `config-virt` for
  7.3.3), so the build needs only the kernel.org tarball and old commits stay buildable.
  `CONFIG_LIBCRC32C` is gone from 7.x; `rekcod.config` asks for `CONFIG_CRC32` instead.
- `KERNEL=alpine` (stock kernel, for bisecting) uses Alpine edge's `linux-virt` of the same
  version, built in an edge chroot.

Verified on an isolated VM: boots to `ready` in 0.42 s of guest time on the first boot,
patched virtio-fs active (`rekcod_stats` counting), elastic memory up, and
`scripts/e2e.sh` (70 checks, sandbox suite included) passed three times.

Still to do: turn off `NUMA`, `CPU_FREQ` and `SCHED_DEBUG` and set `NR_CPUS` to 64 in
`rekcod.config`, keeping only the trims that gain 2% on pgbench. Hardening options stay on.

### Keep pinned versions current (done)

`scripts/bump-versions.py` moves every pin to the newest release, and
`.github/workflows/bump.yml` runs it every Monday and opens or updates the `bump/versions`
PR. Pins it covers: Alpine minirootfs, Docker engine, Linux, Alpine edge's `linux-virt`
(for `KERNEL=alpine`) in `guest/versions.env`, and the Mac's Docker CLI, Compose and
Buildx in `crates/rekcod/src/cli_fetch.rs`. On a new kernel series it replaces
`base.config` from Alpine edge when edge has that series, and lists the fuse rebase under
"Needs a person" in the PR body.

The workflow needs "Allow GitHub Actions to create and approve pull requests" turned on in
the repository settings. A PR opened with `GITHUB_TOKEN` doesn't start other workflows, so
it dispatches `ci.yml` on the branch; e2e still needs the self-hosted Mac runner.

Pins on 2026-11-08: Alpine 3.24.4, Docker 29.9.3, Linux 7.3.3, Compose 5.7.0, Buildx
0.38.2. Helper and test images moved to `alpine:3.24`, sandbox defaults to
`debian:trixie-slim`, and `rekcod x` defaults to Python 3.14.

### Published ports over vmnet

Today a Mac connection to a published port goes client, rekcod daemon, vsock, agent,
container. Measured p50 round trips: 88-104 µs over vsock, 46-53 µs connecting from the
Mac to the guest's vmnet address, 143 µs on Docker Desktop.

The vsock-only rule exists because vsock is a channel between this Mac process and this
VM and nothing else. It doesn't depend on routes, so VPN clients, subnet collisions and
Mac firewalls can't break it, and no other machine or VM can reach it. A container port
reachable on the guest's vmnet address is reachable by other VMs on the same vmnet bridge
(UTM, other Virtualization.framework apps) unless the agent filters by source.

The 46-53 µs number skipped rekcod's own relay: the client connected straight to the guest
address. Published ports still need the daemon's Mac listener on `localhost`, so the real
path would be client, daemon, vmnet, container, and the gain is smaller than the gap
above. The main VM on `--network user` (which `auto` picks on full-tunnel VPNs) has no
vmnet address at all, so those users keep vsock either way.

Prototype first: the daemon dials the guest address for one port, with the source filter
in the agent. Measure `bench/tcp_rtt.py` and pgbench `-S` from the Mac. If p50 doesn't drop
by at least 25 µs, keep vsock only. If it does, ship it as a fast path in NAT mode with
vsock as the fallback, using the safeguards in `git show bdfa761:docs/vmnet-ports.md`
(source filter, probe with a short connect timeout, e2e checks that other VMs can't reach
container ports).

## Sandbox VMs

### Raise the sandbox network MTU

`netstack.rs:35` uses 1500, one datagram per frame through smoltcp. All sandbox, build and
app-instance traffic goes through it, and so does the main VM on `--network user`, which
`auto` picks on full-tunnel VPNs (`cli.rs:66`). I believe the file-handle attachment's
`maximumTransmissionUnit` goes up to 65535. Raise it there, in smoltcp and on the guest's
`eth0`.

Gate: `bench/sandbox_net.sh` at least 2×.

### Skip unchanged files in sandbox diff

`same()` (`sandbox.rs:741`) reads both copies of every same-size file on every diff.
Record each workspace file's ctime right after `clonefile`. The guest can't set ctime and
every write changes it, so a matching ctime means the content hasn't changed and the read
can be skipped.

Gate: `bench/diff.sh`.

### Keep warm VMs ready

`sandbox create`, `job run` and fleet scale-ups each cold-boot a VM. A small pool of booted
VMs from the golden disk, with the workspace share attached when one is handed out, would
cut that. I believe `VZVirtioFileSystemDevice.share` can be changed on a running VM; check
that before designing around it. Pool VMs hold memory that isn't returned to macOS, so the
pool is small and opt-in.

Gate: create-to-first-exec in `bench/sandbox.sh`.

### Distribute golden disks

Fleet nodes get golden disks by hand (`cp -cR ~/.rekcod/golden/default`). Store them in
RustFS by content hash and have a node fetch the one a sandbox asks for.

## Fleet

### Persistent builder per namespace

Each build starts a new VM, downloads `<app>.tar.zst`, unpacks it, builds, then tars the
whole `mode=max` cache and uploads it even when nothing changed (`fleet.rs:586-657`). One
long-lived builder VM per namespace on each node keeps BuildKit's cache and base images on
disk. RustFS is then only needed when a build lands on another node. One per namespace, so
one namespace's builds can't poison another's cache.

Gate: second build of an unchanged commit.

### Recheck the s3 cache backend

The comment on `build_image` (`fleet.rs:616`) says dockerd's builder doesn't support
`type=s3`. With the containerd image store the docker driver supports more cache backends.
If s3 works, uploads become incremental blobs. It needs per-app S3 credentials scoped to a
cache prefix, the way app buckets already get them.

### Layer-deduped releases

Each new instance downloads the whole `docker save | zstd` tarball, base layers included,
and runs `docker load` (`push_image` at `fleet.rs:541`, `load_image` at `fleet.rs:561`).
Store layers by digest with a blob cache on each node, sharing code with the pull cache.
If the next item ships, this one mostly stops mattering.

### Boot app instances from the image

Instances don't need a Docker API. The build node flattens the release image into an ext4
or EROFS disk once. Nodes cache that disk and APFS-clone it per instance. `rekcod-agent`
runs as PID 1, applies the image's env, user, workdir and entrypoint, starts the app and
captures its output for `app logs`. That removes dockerd and containerd from every
instance, and `docker load` from every scale-up and wake. Build VMs, sandboxes and jobs
keep Docker.

Gate: idle footprint per instance against the ~280 MB measured for a full VM, and wake
latency after `idle_stop`.

### Dial local instances directly from the edge

On one server a request goes edge, node relay (`fleet.rs:430`), the sandbox daemon's
loopback port, vsock, agent, container. The relay exists to authenticate remote edges.
When the edge and node share a Mac, the edge can dial the loopback port.

Gate: p50 latency through the edge.

### Push routes to edges

Edges fetch `/routes` every 2 s (`edge/mod.rs:276`), so a deploy's traffic switch can take
up to 2 s to arrive. A long poll would make it immediate. Low value.

## Upgrades and front ends

### Upgrade without restarting the VM

The VM lives in the daemon process, so a host-side upgrade restarts it and removes `--rm`
containers. Split off a small VM-holder process that rarely changes, and let the agent be
replaced and re-executed without a reboot. This is the largest item here.

### Dashboard without Node.js

The dashboard needs Node.js at runtime (`dashboard.rs:56`). A static export served by the
rekcod binary removes that requirement, with its server parts ported into the binary.

## Decided against

- Replacing dockerd, BuildKit or Compose.
- A custom file server: 63 µs per request against 46 µs for virtio-fs.
- libkrun: loses Rosetta.
- VZ save/restore for `rekcod stop`: doesn't work with virtio-fs.
- 16K guest pages: they break jemalloc in prebuilt images.
- Turning off kernel hardening: about 3%, and the guest kernel is the boundary for
  untrusted images.
- `init_on_free=1` and the traditional balloon: neither returns memory to macOS.

Not available on VZ: DAX for virtio-fs, page sharing between VMs.

## Decisions

Made on 2026-11-08:

- Kernel: newest stable series on its newest point release (7.3 now), base config
  vendored in the repo. Pinned versions bumped by a weekly PR.
- Reflink volumes: a separate XFS volumes disk, with existing volumes copied there once.
- vmnet ports: prototype and measure the relayed path before deciding.

Still open:

- App instances without dockerd: `app logs` would read the agent's capture instead of
  `docker logs`.

## Order of work

Each stage ships on its own. Every change updates tests and the README in the same commit;
new user-facing commands get a `Kind` in `actions.rs`, and the parity tests must pass.

1. Benchmarks: write the missing ones and record baselines here.
2. Host-only release: diff ctime skip, batched import, edge direct dial, golden disk
   distribution.
3. One guest release for the cheap guest changes: user daemon.json, log rotation, build
   cache GC, fd limits, live-restore, Rosetta cache, autoports scan, host-network ports,
   MTU, and the agent's freeze command. Concurrent downloads only if `bench/pull.sh` says
   so.
4. Whole-engine checkpoint, on top of the freeze command from stage 3. The vmnet
   prototype and its measurement.
5. Kernel release: trims, the XFS volumes disk and its migration, disk device and ext4
   options per fio, the vCPU experiment. (Linux 7.3, the vendored config, Alpine 3.24,
   Docker 29.9.3 and the weekly bump job are done.)
6. Pull cache and layer store, one blob store for both.
7. Fleet builds: persistent builder, s3 recheck, warm VM pool.
8. App instances booted from the image.
9. Upgrades without a VM restart, the dashboard without Node, then whichever measure-first
   items the benchmarks justified (docker-proxy stub, idle wakeups, file watch scope,
   containerd store size).
