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

No docker CLI? `rekcod` is one. Any docker command works after `rekcod`, boots the VM if
it's off, and always targets rekcod whatever context is active:

```sh
rekcod run --rm -it alpine sh
rekcod compose up -d
rekcod ps
```

If `docker` isn't on your `PATH`, rekcod downloads docker 29.9.0, compose and buildx
(sha256-pinned) to `~/.rekcod/cli` on first use. Details: [`docs/cli-passthrough.md`](docs/cli-passthrough.md).

## Install

From the latest GitHub release:

```sh
curl -fsSL https://github.com/TimMikeladze/rekcod/releases/latest/download/rekcod-darwin-arm64.tar.gz | tar -xz
sudo mv bin/rekcod /usr/local/bin/
xattr -d com.apple.quarantine /usr/local/bin/rekcod 2>/dev/null   # not notarized yet
rekcod start      # first run downloads and verifies the guest image (~100 MB)
```

The binary pins the guest it was released with (`~/.rekcod/guest/<version>/`) and
checks its sha256 before unpacking. `REKCOD_RELEASE_URL` points the download elsewhere.

## What works

Verified by `scripts/e2e.sh` (47 checks) and `tests/stress/` on an M5 Pro, macOS 27:

- Upstream dockerd 29.9.0 with its embedded BuildKit. `docker build`, compose `build:` and `buildx bake` all work.
- Published TCP and UDP ports on the Mac's `localhost`, mirrored from docker events. If a Mac port is already taken, rekcod reports it in `rekcod status` and leaves the other process alone.
- Bind mounts from `/Users`, `/Volumes`, `/private`, `/tmp` and `/var/folders` at the same paths, **cached in the
  guest and invalidated from FSEvents**. Warm work is 3–9× faster than OrbStack's bind
  mounts and faster than native macOS (table below). Mac-side edits, atomic saves, creates,
  deletes and chmods reach containers within about 14 ms and **fire inotify**, so hot reload works.
  Case-insensitive names behave like on the Mac (`Foo` is visible as `foo`). A bind-mounted
  tree is pre-warmed in the background from the moment its container is created, and the
  most recently used ones are warmed again right after `rekcod start`.
- `linux/amd64` images through Rosetta.
- `host.docker.internal` (including Mac services bound only to `127.0.0.1`) and `gateway.docker.internal`. Container DNS goes through the Mac's own resolvers (UDP and TCP), so VPN and link-local DNS work.
- `docker run -i`/`attach`/`exec` with stdin half-close, healthcheck-gated `depends_on`, and idempotent `compose up`.
- `rekcod import`: images (`save | load`) and named volumes (data, labels; copied with `docker cp` and checked by file count and bytes), so compose reuses them. Volumes in use at the source are skipped unless you pass `--force`.
- Idle VM memory is about 160 MiB RSS.
- Database-style workloads run at native-Mac durability: guest `fsync` maps to macOS `fsync` (survives VM or app crashes, like any Mac app), not a full media flush on every commit. pgbench (8 clients, scale 20): about 15k TPS, versus 1k with Virtualization.framework's default. `REKCOD_DISK_SYNC=full` restores full flushes.
- The data disk is sparse (256 GiB by default, grown in place by `--disk`), and the build cache is capped at 20 GB.

## Beyond Docker Desktop

**VM-local directories.** A container label keeps dependency and build directories
inside the VM, so `npm install`, `rm -rf node_modules` and build output never cross into
macOS. The Mac sees an empty directory there.

```yaml
services:
  web:
    build: .
    volumes: [.:/app]
    working_dir: /app
    labels:
      dev.rekcod.local: "node_modules,.next"   # relative to working_dir, or absolute
```

rekcod's socket proxy adds a named volume per path (`rekcod-local_<project>_<service>_<path>`)
when the container is created, so the contents survive `compose up --force-recreate`.
`docker volume rm` them to start over.

**Volume snapshots.** Save a seeded database once, get back to it in a second:

```sh
rekcod snap save seeded        # pauses the project's containers, copies its volumes
rekcod snap restore seeded     # stops them, copies back, starts them again
rekcod snap save               # no name: the current git branch, for per-branch state
rekcod snap ls [--all] [--json]
rekcod snap rm seeded
```

The project is the compose project started from the current directory (`-p` to pick one).
Snapshots are copies on the VM's disk (a 360 MB Postgres data dir saves or restores in
under a second). A restore after `compose down -v` recreates the volumes so compose adopts them.

**Run anything without a Dockerfile.** `rekcod x <command>` picks the image from the
command and the project's version files (`.nvmrc`, `package.json` engines, `bun.lock`,
`.python-version`, `pyproject.toml`, `uv.lock`, `go.mod`, `Cargo.toml`, `.tool-versions`,
`mise.toml`, ...), mounts the current directory at the same path, and keeps `node_modules`,
`.venv` and `target` on VM volumes so Mac and Linux builds of native modules never mix.
Ports the command listens on, even on `127.0.0.1`, show up on the Mac's `localhost`.

```sh
rekcod x npm install && rekcod x npm run dev     # node:<your version>-slim
rekcod x uv run pytest                          # uv image with your Python
rekcod x cargo test                             # rust image, target/ stays in the VM
rekcod x --image alpine:3.22 sh                 # any image
```

`-p PORT` publishes explicitly, `-e KEY=VALUE` sets variables, and `.env` is passed along.
A warm `rekcod x node -v` adds about 65 ms over running node natively.

**Menu bar.** `rekcod menubar` shows docker's whale upside down (dimmed while the VM is stopped). Its menu lists the VM, containers, forwarded ports and compose
projects with their snapshots, with the same actions as the CLI: start/stop the VM,
compact, root shell; start, stop, restart, logs or remove a container; open a port;
save, restore or delete a snapshot. Every item runs the matching `rekcod` command, and
anything destructive asks first. How it stays in sync with the CLI: [`docs/frontends.md`](docs/frontends.md).

**Sandbox.** `rekcod x --sandbox <command>` runs in a second, throwaway VM whose only Mac
directory is the current one. The rest of `$HOME` (`~/.ssh`, cloud credentials, other
repos) doesn't exist there, and nothing on the Mac or the local network is reachable;
the internet is. Variables get in only with `--secret NAME`; `--offline` cuts the network.
Use it for `npm install` of something you don't trust, or for an AI agent's commands.
Threat model and limits: [`docs/sandbox.md`](docs/sandbox.md).

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
| `rekcod start [--cpus N] [--memory GiB] [--disk GiB] [--no-switch]` | Boot the VM, create and select the `rekcod` docker context. `--cpus` defaults to the Mac's performance cores (at least 4); `--disk` (default 256, sparse) grows the data disk, never shrinks it |
| `rekcod stop [--no-switch]` | Graceful power-off, restore the previous context |
| `rekcod status [--json]` | Versions, VM size, forwarded ports (and any that couldn't be bound), data-disk use in the VM and on the Mac, file-cache trips to macOS, warming in progress, the sandbox VM. `--json` is the scriptable form |
| `rekcod snap save\|restore\|ls\|rm [NAME] [-p PROJECT]` | Volume snapshots of a compose project (above) |
| `rekcod x [--sandbox] [--image IMG] [-p PORT] <command>` | Run a command in a container, no Dockerfile (above) |
| `rekcod socket install\|uninstall` | Route `/var/run/docker.sock` to rekcod while it runs (sudo once) |
| `rekcod import --from <context> [NAMES...] [--images-only\|--volumes-only] [--force]` | Copy images and named volumes from another engine |
| `rekcod shell` | Root shell in the VM |
| `rekcod menubar [--install\|--uninstall]` | Menu bar item with the same actions as the CLI: VM start/stop/compact/shell, containers (start, stop, restart, logs, remove), ports (click to open), snapshots (save, restore, delete). `--install` starts it at login (LaunchAgent `dev.rekcod.menubar`) |
| `rekcod compact` | Give deleted image/volume space back to macOS now (also runs hourly) |
| `rekcod <docker command>` | Runs `docker <command>` against rekcod (`ps`, `run`, `compose`, `build`, ...) |
| `rekcod docker <args>` | Always docker, e.g. `rekcod docker status` |

`start`/`stop` with a container name or docker flag (`rekcod stop -t 5 web`) and `import`
without `--from` are docker's. `REKCOD_DOCKER_CLI=/path/to/docker` picks the CLI.
Clients that ignore docker contexts (oxker, SDKs, IDE plugins) dial `/var/run/docker.sock`.
The first `rekcod start` in a terminal sets this up (one password prompt; or run `rekcod socket install` later). From then on `rekcod start` routes
that socket to rekcod and `rekcod stop` hands it back to whatever had it (OrbStack, Docker Desktop).
`rekcod socket uninstall` undoes it. See [docs/default-socket.md](docs/default-socket.md).
Or per shell: `export DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock`.
If `DOCKER_HOST` is already set, it overrides contexts, and `rekcod start` warns you.

## Build

Needs Rust, Xcode command-line tools (`codesign`), and any docker engine for the
first guest image build (it compiles the guest kernel, so the first build takes a few minutes). After that, rekcod can rebuild its own guest.

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
cargo fmt --all --check                      # formatting (rustfmt.toml; CI enforces it)
scripts/e2e.sh                               # real CLI + compose against rekcod
E2E_IMPORT_FROM=orbstack scripts/e2e.sh      # also exercises import
python3 bench/headroom.py --context rekcod   # file-sharing benchmark (compare --context orbstack)
python3 bench/inotify_rtt.py --context rekcod  # Mac edit -> container inotify latency
bench/cold_start.sh                          # first access right after boot, warmed vs not
rekcod x cargo test -p rekcod-agent          # the guest agent's Linux-only tests
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

- `crates/rekcod`: CLI plus the daemon that owns the VM (`objc2-virtualization`), the socket proxy (`proxy.rs`, rewrites `containers/create` for `dev.rekcod.local`), the port forwarder (published ports and `rekcod x` listeners), file events, the DNS bridge and clock sync. `snap.rs` and `xrun.rs` are the newer commands; `actions.rs` is the action list that `menubar.rs` renders.
- `crates/rekcod-agent`: static musl guest agent.
- `crates/rekcod-proto`: vsock ports and wire formats.
- `guest/`: a 6.12 LTS kernel built from Alpine's `linux-virt` config plus `guest/kernel/rekcod.config` (virtio, ext4, fuse and docker's netfilter built in, no initramfs; [docs/custom-kernel.md](docs/custom-kernel.md)), an Alpine ext4 root with upstream static docker binaries, and busybox init. `--build-arg KERNEL=alpine` builds the stock-kernel guest instead.
- `guest/fuse/`: the fuse/virtiofs patch (long TTLs, lazy open, host-driven invalidation), applied in-tree to the guest kernel.
- State lives in `~/.rekcod`: `vm/root.img` (replaced on upgrade), `vm/data.img` (256 GiB sparse by default, all docker data), `vm/warm-paths` (bind sources warmed at boot) and `logs/`. The sandbox VM has its own copy under `~/.rekcod/sandbox`.

Design and roadmap: [`docs/mvp.md`](docs/mvp.md), [`docs/plan.md`](docs/plan.md), [`docs/phase2.md`](docs/phase2.md), [`docs/phase2b-4.md`](docs/phase2b-4.md), [`docs/research.md`](docs/research.md), [`docs/phase5.md`](docs/phase5.md), [`docs/sandbox.md`](docs/sandbox.md), [`docs/frontends.md`](docs/frontends.md). Next steps: [`docs/handoff.md`](docs/handoff.md).

## Known gaps

- **First access can be slower than OrbStack** for a tree rekcod hasn't seen since boot, when a container touches it before background warming finishes (about 1 s per 20k files). Recently used trees are warmed at boot (a 19.5k-file walk 3 s after `rekcod start`: 34 ms warmed vs 286 ms not). Repeat work is 3–9× faster.
- Mac→container change latency is about 14 ms p50 (`bench/inotify_rtt.py`; OrbStack measured 12 ms). Changes are visible after that bounded delay, not instantly.
- **Memory isn't returned to macOS.** `REKCOD_BALLOON=1` drives the balloon, but on macOS 27 the VM's footprint doesn't drop, even under memory pressure (docs/phase2.md, docs/phase5.md).
- `--sandbox` hasn't had an external security review (docs/sandbox.md).
- No brew formula. Releases are ad-hoc signed, not notarized.

Debug knobs: `REKCOD_DEBUG=1` logs forwarded file events. `REKCOD_KERNEL_CMDLINE` overrides
the guest command line. `REKCOD_NO_BALLOON=1` drops the balloon device. In the guest, `cat /sys/module/virtiofs/parameters/rekcod_stats`
shows host requests by FUSE opcode.

## License

Apache-2.0, see [`LICENSE`](LICENSE).
