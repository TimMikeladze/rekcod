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

## Install

From a GitHub release (the repo is private for now, so use `gh`):

```sh
gh release download -R TimMikeladze/rekcod -p 'rekcod-*-darwin-arm64.tar.gz'
tar -xzf rekcod-*-darwin-arm64.tar.gz && sudo mv bin/rekcod /usr/local/bin/
xattr -d com.apple.quarantine /usr/local/bin/rekcod 2>/dev/null   # not notarized yet
rekcod start      # first run downloads and verifies the guest image (~100 MB)
```

The binary pins the guest it was released with (`~/.rekcod/guest/<version>/`) and
checks its sha256 before unpacking. `REKCOD_RELEASE_URL` points the download elsewhere.

## What works

Verified by `scripts/e2e.sh` (37 checks) and `tests/stress/` on an M5 Pro, macOS 27:

- Upstream dockerd 29.9.0 with its embedded BuildKit. `docker build`, compose `build:` and `buildx bake` all work.
- Published TCP and UDP ports on the Mac's `localhost`, mirrored from docker events. If a Mac port is already taken, rekcod reports it in `rekcod status` and leaves the other process alone.
- Bind mounts from `/Users`, `/Volumes`, `/private`, `/tmp` and `/var/folders` at the same paths, **cached in the
  guest and invalidated from FSEvents**. Warm work is 3–9× faster than OrbStack's bind
  mounts and faster than native macOS (table below). Mac-side edits, atomic saves, creates,
  deletes and chmods reach containers within about 25 ms and **fire inotify**, so hot reload works.
  Case-insensitive names behave like on the Mac (`Foo` is visible as `foo`). A newly
  bind-mounted tree is pre-warmed in the background, so first access is up to 6× faster.
- `linux/amd64` images through Rosetta.
- `host.docker.internal` (including Mac services bound only to `127.0.0.1`) and `gateway.docker.internal`. Container DNS goes through the Mac's own resolvers (UDP and TCP), so VPN and link-local DNS work.
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
after boot, or after the file changes on the Mac. How it works: [`docs/phase2.md`](docs/phase2.md), [`docs/phase2b-4.md`](docs/phase2b-4.md), [`docs/research.md`](docs/research.md).

## Commands

| Command | |
|---|---|
| `rekcod start [--cpus N] [--memory GiB] [--disk GiB] [--no-switch]` | Boot the VM, create and select the `rekcod` docker context |
| `rekcod stop [--no-switch]` | Graceful power-off, restore the previous context |
| `rekcod status` | Versions, VM size, forwarded ports (and any that couldn't be bound) |
| `rekcod import --from <context> [NAMES...] [--images-only\|--volumes-only] [--force]` | Copy images and named volumes from another engine |
| `rekcod shell` | Root shell in the VM |
| `rekcod compact` | Give deleted image/volume space back to macOS now (also runs hourly) |

Without the docker CLI, run `export DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock`.
If `DOCKER_HOST` is already set, it overrides contexts, and `rekcod start` warns you.

## Build

Needs Rust, Xcode command-line tools (`codesign`), and any docker engine for the
first guest image build. After that, rekcod can rebuild its own guest.

```sh
scripts/build.sh                                   # host binary (signed), agent, out/guest
REKCOD_BUILD_DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock scripts/build.sh   # self-hosted
scripts/build.sh --host-only                       # skip the guest image
scripts/package.sh                                 # release tarballs + SHA256SUMS in out/release
```

Releases: push a `vX.Y.Z` tag matching `Cargo.toml`. `.github/workflows/release.yml`
builds the guest on arm64 Linux and the host on macOS, then publishes the release.
CI (`ci.yml`) runs unit tests, clippy and a guest build. The e2e suite needs a Mac
that can run VMs, so it runs locally.

`target/release/rekcod` finds `out/guest` on its own. Elsewhere, set `REKCOD_GUEST_DIR` or copy
`out/guest` to `~/.rekcod/guest`.

## Test

```sh
cargo test --workspace                       # unit tests
scripts/e2e.sh                               # real CLI + compose against rekcod
E2E_IMPORT_FROM=orbstack scripts/e2e.sh      # also exercises import
python3 bench/headroom.py --context rekcod   # file-sharing benchmark (compare --context orbstack)
python3 tests/stress/fsx.py --ops 500        # random Mac/container file ops must converge
tests/stress/overflow.sh                     # FSEvents burst; see the script for the forced-drop mode
tests/stress/postgres.sh                     # PGDATA on a bind mount: pgbench, restart, kill -9, amcheck
tests/stress/slow-reader.sh                  # a stalled docker client must not freeze the VM
tests/stress/compose-e2e.sh rekcod orbstack  # docker/compose's own e2e suite
```

## How it works

```
docker CLI ─▶ ~/.rekcod/run/docker.sock ─▶ rekcodd ──vsock──▶ rekcod-agent ─▶ dockerd
Mac :8080  ─▶ rekcodd listener ─────────────────vsock──▶ agent ─▶ 127.0.0.1:8080 (docker-proxy)
container ─▶ host.docker.internal:N ─iptables─▶ agent ──parked vsock──▶ rekcodd ─▶ Mac 127.0.0.1:N
FSEvents   ─▶ rekcodd ──vsock──▶ agent ─▶ fuse rekcod_notify: invalidate + inotify
guest DNS  ─▶ dnsmasq ─▶ agent stub ──vsock──▶ rekcodd ─▶ Mac resolvers
```

- `crates/rekcod`: CLI plus the daemon that owns the VM (`objc2-virtualization`), the socket proxy, the port forwarder, file events, the DNS bridge and clock sync.
- `crates/rekcod-agent`: static musl guest agent.
- `crates/rekcod-proto`: vsock ports and wire formats.
- `guest/`: Alpine `linux-virt` 6.12 kernel and initramfs, an ext4 root with upstream static docker binaries, and busybox init.
- `guest/fuse/`: patched `fuse`/`virtiofs` modules (long TTLs, lazy open, host-driven invalidation), built against the guest kernel.
- State lives in `~/.rekcod`: `vm/root.img` (replaced on upgrade), `vm/data.img` (64 GiB sparse, all docker data) and `logs/`.

Design and roadmap: [`docs/mvp.md`](docs/mvp.md), [`docs/plan.md`](docs/plan.md), [`docs/phase2.md`](docs/phase2.md), [`docs/phase2b-4.md`](docs/phase2b-4.md), [`docs/research.md`](docs/research.md). Next steps: [`docs/handoff.md`](docs/handoff.md).

## Known gaps

- **First access can be slower than OrbStack** when a container touches files before background warming finishes (about 1 s per 20k files). Repeat work is 3–9× faster.
- Mac→container change latency is about 25 ms (OrbStack: about 10 ms). Changes are visible after that bounded delay, not instantly.
- **Memory isn't returned to macOS.** `REKCOD_BALLOON=1` drives the balloon, but on macOS 27 the VM's footprint doesn't drop (docs/phase2.md, docs/research.md).
- No menu bar app, no brew formula. Releases are ad-hoc signed, not notarized.

Debug knobs: `REKCOD_DEBUG=1` logs forwarded file events. `REKCOD_KERNEL_CMDLINE` overrides
the guest command line. `REKCOD_NO_BALLOON=1` drops the balloon device. In the guest, `cat /sys/module/virtiofs/parameters/rekcod_stats`
shows host requests by FUSE opcode.
