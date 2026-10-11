# rekcod

Docker on macOS (Apple Silicon): the real, unmodified `dockerd` in a lightweight
Virtualization.framework VM, switched to in one command and away from in one command.
Your `docker` CLI, `docker compose` and compose files stay exactly as they are.

```sh
rekcod start                          # boots in ~1.3 s, selects the `rekcod` docker context
rekcod import --from desktop-linux    # or --from orbstack: images + named volumes
docker compose up -d                  # unchanged
rekcod stop                           # powers off, restores your previous context
```

## What works

Verified by `scripts/e2e.sh` (28 checks) on an M5 Pro, macOS 27:

- Upstream dockerd 29.9.0 with its embedded BuildKit. `docker build`, compose `build:` and `buildx bake` all work.
- Published ports on the Mac's `localhost`, mirrored from docker events. If a Mac port is already taken, rekcod reports it in `rekcod status` and leaves the other process alone.
- Bind mounts from `/Users`, `/Volumes`, `/private` and `/var/folders` at the same paths, **cached in the
  guest and invalidated from FSEvents**. Warm work is 3–9× faster than OrbStack's bind
  mounts and faster than native macOS (table below). Mac-side edits, atomic saves, creates,
  deletes and chmods reach containers within about 25 ms and **fire inotify**, so hot reload works.
- `linux/amd64` images through Rosetta.
- `host.docker.internal` and `gateway.docker.internal`. Container DNS goes through the Mac's own resolvers, so VPN and link-local DNS work.
- `docker run -i`/`attach`/`exec` with stdin half-close, healthcheck-gated `depends_on`, and idempotent `compose up`.
- `rekcod import`: images (`save | load`) and named volumes (data, labels), so compose reuses them. Volumes in use at the source are skipped unless you pass `--force`.
- Idle VM memory is about 160 MiB RSS.

## File-sharing speed

`bench/headroom.py`, 20.5k files (`#2` = warm repeat):

| | rekcod bind | OrbStack bind | native macOS |
|---|---|---|---|
| list + stat (`walk#2`) | **34–47 ms** | 142 ms | 110 ms |
| missing-file lookups (`neg#2`) | **89–112 ms** | 668 ms | 232 ms |
| open + read (`read#2`) | **~105 ms** | 909 ms | 510 ms |
| first (cold) read of the tree | 1.7 s | 0.84 s | 0.45 s |

The guest keeps its caches across container runs. The cold cost is paid once per file
after boot, or after the file changes on the Mac. How it works: [`docs/phase2.md`](docs/phase2.md).

## Commands

| Command | |
|---|---|
| `rekcod start [--cpus N] [--memory GiB] [--disk GiB] [--no-switch]` | Boot the VM, create and select the `rekcod` docker context |
| `rekcod stop [--no-switch]` | Graceful power-off, restore the previous context |
| `rekcod status` | Versions, VM size, forwarded ports (and any that couldn't be bound) |
| `rekcod import --from <context> [NAMES...] [--images-only\|--volumes-only] [--force]` | Copy images and named volumes from another engine |
| `rekcod shell` | Root shell in the VM |

Without the docker CLI, run `export DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock`.
If `DOCKER_HOST` is already set, it overrides contexts, and `rekcod start` warns you.

## Build

Needs Rust, Xcode command-line tools (`codesign`), and any docker engine for the
first guest image build. After that, rekcod can rebuild its own guest.

```sh
scripts/build.sh                                   # host binary (signed), agent, out/guest
REKCOD_BUILD_DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock scripts/build.sh   # self-hosted
scripts/build.sh --host-only                       # skip the guest image
```

`target/release/rekcod` finds `out/guest` on its own. Elsewhere, set `REKCOD_GUEST_DIR` or copy
`out/guest` to `~/.rekcod/guest`.

## Test

```sh
cargo test --workspace                       # unit tests
scripts/e2e.sh                               # real CLI + compose against rekcod
E2E_IMPORT_FROM=orbstack scripts/e2e.sh      # also exercises import
python3 bench/headroom.py --context rekcod   # file-sharing benchmark (compare --context orbstack)
```

## How it works

```
docker CLI ─▶ ~/.rekcod/run/docker.sock ─▶ rekcodd ──vsock──▶ rekcod-agent ─▶ dockerd
Mac :8080  ─▶ rekcodd listener ─────────────────vsock──▶ agent ─▶ 127.0.0.1:8080 (docker-proxy)
FSEvents   ─▶ rekcodd ──vsock──▶ agent ─▶ fuse rekcod_notify: invalidate + inotify
guest DNS  ─▶ dnsmasq ─▶ agent stub ──vsock──▶ rekcodd ─▶ Mac resolvers
```

- `crates/rekcod`: CLI plus the daemon that owns the VM (`objc2-virtualization`), the socket proxy, the port forwarder, file events, the DNS bridge and clock sync.
- `crates/rekcod-agent`: static musl guest agent.
- `crates/rekcod-proto`: vsock ports and wire formats.
- `guest/`: Alpine `linux-virt` 6.12 kernel and initramfs, an ext4 root with upstream static docker binaries, and busybox init.
- `guest/fuse/`: patched `fuse`/`virtiofs` modules (long TTLs, lazy open, host-driven invalidation), built against the guest kernel.
- State lives in `~/.rekcod`: `vm/root.img` (replaced on upgrade), `vm/data.img` (64 GiB sparse, all docker data) and `logs/`.

Design and roadmap: [`docs/mvp.md`](docs/mvp.md), [`docs/plan.md`](docs/plan.md).

## Known gaps

- **First access is slower than OrbStack.** Cold, first-touch bind-mount work is 2–6× slower, because Apple's virtio-fs pays a high per-request cost. Repeat work is 3–9× faster.
- Mac→container change latency is about 25 ms (OrbStack: about 10 ms). Changes are visible after that bounded delay, not instantly.
- UDP port publishing isn't forwarded to the Mac.
- `host.docker.internal` reaches Mac services listening on all interfaces, not ones bound only to `127.0.0.1`.
- Mac `/tmp` isn't shared. Use `/private/tmp`.
- DNS answers larger than 4 KB (TCP fallback) aren't supported.
- **Memory isn't returned to macOS.** `REKCOD_BALLOON=1` drives the balloon, but on macOS 27 the VM's footprint doesn't drop (docs/phase2.md).
- No menu bar app, no brew formula.

Debug knobs: `REKCOD_DEBUG=1` logs forwarded file events. `REKCOD_KERNEL_CMDLINE` overrides
the guest command line. In the guest, `cat /sys/module/virtiofs/parameters/rekcod_stats`
shows host requests by FUSE opcode.
