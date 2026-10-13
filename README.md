# rekcod

rekcod runs Docker on Apple Silicon Macs: the unmodified upstream `dockerd` in a
Virtualization.framework VM. `rekcod start` switches your docker context to it and
`rekcod stop` switches back. Your `docker` CLI, `docker compose` and compose files don't change.

```sh
rekcod start                          # boots in ~1.3 s, selects the `rekcod` docker context
rekcod import --from desktop-linux    # or --from orbstack: images + named volumes
docker compose up -d                  # unchanged
rekcod stop                           # powers off, restores your previous context
```

`rekcod` also works as the docker CLI. Put any docker command after it. The VM boots if
it's off, and the command targets rekcod whatever context is active:

```sh
rekcod run --rm -it alpine sh
rekcod compose up -d
rekcod ps
```

If `docker` isn't on your `PATH`, rekcod downloads docker 29.9.0, compose and buildx
(sha256-pinned) to `~/.rekcod/cli` on first use. Details: [`docs/cli-passthrough.md`](docs/cli-passthrough.md).

## Why rekcod

rekcod doesn't reimplement Docker. It runs the same upstream `dockerd` 29.9.0 you'd run on a
Linux server, with its embedded BuildKit, so compose, `buildx bake`, SDKs and anything else
that talks to the Docker API behave as they do there. All of rekcod's own code sits at the VM
boundary (file sharing, ports, DNS, memory), because that's where Docker on a Mac is slow or
breaks.

Bind mounts are where you feel it. rekcod patches the guest kernel's FUSE/virtio-fs to cache
file metadata until FSEvents says the file changed on the Mac. Warm work on a bind-mounted
tree is 3–9× faster than OrbStack and faster than the same work natively on macOS
([numbers](#file-sharing-speed)). The cost is the first read of a tree after boot, which is
slower than OrbStack ([Limitations](#limitations)).

On top of stock Docker it adds volume snapshots per compose project or git branch,
`rekcod x` for running a command in the right toolchain image without a Dockerfile, a
throwaway sandbox VM for code you don't trust, and VM-local `node_modules`
([Beyond Docker Desktop](#beyond-docker-desktop)).

It's Apache-2.0. Colima, Lima, Podman Desktop and Rancher Desktop are open source too. What
rekcod is going for is open source at OrbStack's speed.

## Goals

- Switch from Docker Desktop or OrbStack in one command, bring images and volumes along with
  `rekcod import`, and switch back in one command. Compose files and scripts don't change.
- Full Docker API parity, by running upstream `dockerd` instead of reimplementing the engine,
  Compose or BuildKit.
- Be faster than OrbStack on what developers notice day to day, which is mostly bind-mounted
  file access.
- Dev-loop features that work on stock `dockerd`: snapshots, `rekcod x`, the sandbox.
- The CLI and the menu bar offer the same actions ([`docs/frontends.md`](docs/frontends.md)).

Not goals: Windows or Linux hosts, 16K-page guest kernels, lazy image pulls, or a microVM per
container (only `--sandbox` gets its own VM). The reasoning behind these is in
[`docs/plan.md`](docs/plan.md).

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
- Published TCP and UDP ports on the Mac's `localhost`, mirrored from docker events. If a Mac port is already taken, rekcod reports it in `rekcod status` and leaves the other process alone. Mac TCP connections go straight to the container's address, skipping docker-proxy (about 85 µs round trip vs 125 µs through it), so iptables rules in the guest's `DOCKER-USER` chain don't apply to them.
- Bind mounts from `/Users`, `/Volumes`, `/private`, `/tmp` and `/var/folders` at the same paths, cached in the
  guest and invalidated from FSEvents. Warm work is 3–9× faster than OrbStack's bind
  mounts and faster than native macOS (table below). Mac-side edits, atomic saves, creates,
  deletes and chmods reach containers within about 14 ms and fire inotify, so hot reload works.
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

### VM-local directories

A container label keeps dependency and build directories
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

### Volume snapshots

Save a seeded database once, get back to it in a second:

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

### Run anything without a Dockerfile

`rekcod x <command>` picks the image from the
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

### Menu bar

`rekcod menubar` shows docker's whale upside down (dimmed while the VM is stopped). Its
menu lists the VM, containers, forwarded ports and compose projects with their snapshots,
with the same actions as the CLI: start/stop the VM,
compact, root shell; start, stop, restart, logs or remove a container; open a port;
save, restore or delete a snapshot; open a shell in an agent sandbox, diff, apply, stop or delete it. Every item runs the matching `rekcod` command, and
anything destructive asks first. How it stays in sync with the CLI: [`docs/frontends.md`](docs/frontends.md).

### Sandbox

`rekcod x --sandbox <command>` runs in a second, throwaway VM whose only Mac
directory is the current one. The rest of `$HOME` (`~/.ssh`, cloud credentials, other
repos) doesn't exist there, and nothing on the Mac or the local network is reachable;
the internet is. Variables get in only with `--secret NAME`; `--offline` cuts the network.
Use it for `npm install` of something you don't trust, or for an AI agent's commands.
Threat model and limits: [`docs/sandbox.md`](docs/sandbox.md).

### GitHub Actions runner

A self-hosted runner can live in the VM as a container, using the VM's `dockerd` for jobs.
Compose file and a sample workflow: [`examples/github-runner`](examples/github-runner).

### Agent sandboxes

`rekcod sandbox` gives an AI agent throwaway VMs, each a full `dockerd` that sees a
copy-on-write copy of one project and nothing else on the Mac. The agent works on the copy;
you look at the diff and apply it. Spec and design: [`docs/agent-sandboxes.md`](docs/agent-sandboxes.md).

```sh
rekcod sandbox golden build          # once: a data disk with common images, cloned per sandbox
id=$(rekcod sandbox create)          # this directory; ~1.3 s to a first exec
rekcod sandbox exec $id -- npm test  # exits with the command's code
rekcod sandbox fork $id              # new sandbox from its disk + workspace (~1.2 s)
rekcod sandbox diff $id              # the copy against your project
rekcod sandbox apply $id             # write the changes back (refuses files you edited since)
rekcod sandbox rm $id
```

The internet is limited to an allowlist of package and image registries and GitHub
(`--allow-host` adds names, `--allow-host '*'` opens it, `--offline` cuts it).
Names off the list don't resolve, and connections to addresses no allowed name resolved to
are rejected; `rekcod sandbox log ID --net` shows what was blocked. A sandbox powers off
after 30 minutes without an exec (`--ttl`) and boots again on the next one.

For agents: `rekcod mcp` is an MCP server on stdio with tools for create, exec,
read/write file, fork, diff, apply, stop and remove (`claude mcp add rekcod -- rekcod mcp`).
`rekcod sandbox serve` serves the same operations over HTTP on
`~/.rekcod/run/sandbox.sock` (`--listen 127.0.0.1:PORT` adds TCP with a bearer token):

```sh
curl --unix-socket ~/.rekcod/run/sandbox.sock -X POST localhost/sandboxes/$id/exec -d '{"cmd":["npm","test"]}'
{"stdout":"..."}
{"exit":0}
```

Measured on an M5 Pro (`bench/sandbox.sh`): create to first exec 1.3 s p50, fork 1.2 s,
`rm` 0.12 s, 16 created at once all ready in 3.2 s, about 300 MB idle each.

### macOS sandboxes

`rekcod sandbox create --os macos` clones a macOS base image instead: for iOS and macOS
builds, or an agent driving a real macOS app. Apple's licence allows two per Mac.

```sh
rekcod macos fetch && rekcod macos install   # ~25 GB download, then the installer
rekcod macos setup                           # once, by hand: user "rekcod", Remote Login
rekcod macos authorize                       # from another terminal while setup runs
id=$(rekcod sandbox create --os macos)
rekcod sandbox exec $id -- xcodebuild -version
rekcod sandbox screenshot $id shot.png --open
rekcod sandbox input $id type "hello"
```

exec, diff, apply and fork work as for Linux sandboxes (over SSH; the workspace is copied
into the guest's `~/ws`). The MCP server adds `sandbox_screenshot` and `sandbox_input` for
computer-use agents. Details: [`docs/platform.md`](docs/platform.md#track-4-as-built).

### Fleet and app hosting

The same sandboxes can run on a team's own Macs, and apps can be deployed onto them. One
controller (state in Postgres) places sandboxes and app instances on nodes; `rekcod edge`
routes `<app>.<domain>` to them. Design: [`docs/platform.md`](docs/platform.md). Read the
licence section there first: Apple's macOS licence restricts running it as a service.

```sh
# the controller (any Mac), prints where its admin and join tokens are
rekcod controller serve --db postgres://... --listen 0.0.0.0:7700 --domain apps.example.com
# each Mac
rekcod node join http://controller:7700 "$(cat join-token)" --listen 0.0.0.0:7701 --advertise http://mac2:7701
# you
rekcod remote add team http://controller:7700 --token ADMIN_TOKEN
rekcod controller token alice --max-sandboxes 5          # a user token
rekcod sandbox --remote create                           # clones this repo's origin at HEAD on a node
rekcod sandbox --remote exec ID -- npm test
rekcod edge serve --listen 100.x.y.z:8080 --public-listen 127.0.0.1:8081   # all apps / public apps only
rekcod app deploy                                        # this repo's pushed HEAD + rekcod.toml
rekcod app rollback dash
rekcod app env set dash NODE_ENV=production              # redeploys the live release
rekcod app public dash                                   # also serve it on --public-listen
```

Apps are private until you make them public (`app public NAME`, `app deploy --public`, or
`public = true` under `[app]` in `rekcod.toml`). `--listen` routes every app; bind it to
your tailnet or loopback. `--public-listen` routes only public apps; point a tunnel at it,
for example `cloudflared` with `ingress: [{hostname: "*.example.com", service:
http://127.0.0.1:8081}]` and the controller started with `--domain example.com`. A private
app answers `no app at ...` there, the same as one that doesn't exist. PR previews are
private.

A deploy starts new instances (on different nodes when it can), waits for the health check,
switches the route, then stops the old ones after `--drain` (30 s). A failed health check
leaves the old release serving and prints the new instances' last log lines. A repo without
a Dockerfile gets one from its version files and `rekcod.toml`:

```toml
[app]
port = 8000
start = "python3 app.py"
health = "/healthz"
[resources]
instances = 2
memory = "1GiB"
[env]
GREETING = "hi"
```

Point a GitHub webhook (pull_request events, the secret in `controller/webhook-secret`) at
`/webhooks/github` and each PR gets `<app>-pr-<n>`, deleted when it closes or after 7 days.
`rekcod node` replaces `docker node` here; use `rekcod docker node` for swarm.

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
| `rekcod sandbox create\|exec\|shell\|cp\|fork\|diff\|apply\|ports\|log\|ls\|stop\|rm\|serve\|golden` | Agent sandboxes (above) |
| `rekcod mcp` | MCP server on stdio for the sandbox commands |
| `rekcod macos fetch\|install\|setup\|authorize\|ls\|rm` | macOS base images for `sandbox create --os macos` |
| `rekcod sandbox screenshot\|input` | Screen and mouse/keyboard of a macOS sandbox |
| `rekcod remote add\|ls\|rm` | Fleet controllers this CLI talks to (`--remote` on `sandbox` and `app`) |
| `rekcod node join\|serve\|ls\|drain` | Run this Mac as a fleet node; list or drain nodes |
| `rekcod controller serve\|token` | The fleet controller; create user tokens |
| `rekcod edge serve` | HTTP ingress for apps |
| `rekcod app deploy\|ls\|status\|public\|private\|rollback\|scale\|env\|secret\|logs\|open\|destroy` | Apps on the fleet |
| `rekcod socket install\|uninstall` | Route `/var/run/docker.sock` to rekcod while it runs (sudo once) |
| `rekcod import --from <context> [NAMES...] [--images-only\|--volumes-only] [--force]` | Copy images and named volumes from another engine |
| `rekcod shell` | Root shell in the VM |
| `rekcod menubar [--install\|--uninstall]` | Menu bar item with the same actions as the CLI: VM start/stop/compact/shell, containers (start, stop, restart, logs, remove), ports (click to open), snapshots (save, restore, delete), agent sandboxes (shell, diff, apply, stop, delete), apps on the default remote (open, logs, roll back, destroy). `--install` starts it at login (LaunchAgent `dev.rekcod.menubar`) |
| `rekcod compact` | Give deleted image/volume space back to macOS now (also runs hourly) |
| `rekcod <docker command>` | Runs `docker <command>` against rekcod (`ps`, `run`, `compose`, `build`, ...) |
| `rekcod docker <args>` | Always docker, e.g. `rekcod docker status` |

`start`/`stop` with a container name or docker flag (`rekcod stop -t 5 web`) and `import`
without `--from` are docker's. `REKCOD_DOCKER_CLI=/path/to/docker` picks the CLI.

Clients that ignore docker contexts (oxker, SDKs, IDE plugins) dial `/var/run/docker.sock`.
The first `rekcod start` in a terminal sets that up with one password prompt, or run
`rekcod socket install` later. From then on `rekcod start` routes that socket to rekcod and
`rekcod stop` hands it back to whatever had it (OrbStack, Docker Desktop).
`rekcod socket uninstall` undoes it. See [docs/default-socket.md](docs/default-socket.md).
To point one shell at rekcod instead: `export DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock`.
If `DOCKER_HOST` is already set, it overrides contexts, and `rekcod start` warns you.

## Build

Needs Rust and Xcode command-line tools (`codesign`). No Docker engine: `guest/build.sh`
builds the guest image in chroots inside the running rekcod VM (`rekcod start` first;
a fresh install boots the released guest). The first build compiles the guest kernel;
later builds reuse it until `guest/kernel`, `guest/fuse` or `guest/build.sh` change.
See `docs/guest-build.md`.

```sh
scripts/build.sh                                   # host binary (signed), agent, out/guest
KERNEL=alpine scripts/build.sh                     # Alpine's stock linux-virt instead
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

`git config core.hooksPath .githooks` once per clone runs CI's host checks (fmt, clippy, tests) before every commit.

```sh
cargo test --workspace                       # unit tests
cargo fmt --all --check                      # formatting (rustfmt.toml; CI enforces it)
scripts/e2e.sh                               # real CLI + compose against rekcod
E2E_IMPORT_FROM=orbstack scripts/e2e.sh      # also exercises import
python3 bench/headroom.py --context rekcod   # file-sharing benchmark (compare --context orbstack)
python3 bench/inotify_rtt.py --context rekcod  # Mac edit -> container inotify latency
bench/cold_start.sh                          # first access right after boot, warmed vs not
bench/postgres.sh rekcod desktop-linux       # pgbench on a named volume, per docker context
python3 bench/tcp_rtt.py --context rekcod    # 64-byte round trip through a published port
bench/sandbox.sh                             # agent sandbox create/fork/rm timings, 16 at once
scripts/e2e-sandbox.sh                       # agent sandbox checks only; leaves the main VM alone
scripts/e2e-macos.sh                         # macOS sandbox checks; needs a ready base image
REKCOD_E2E_DB=postgres://... scripts/e2e-fleet.sh   # controller, 2 nodes, edge, apps; WIPES that database
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

- `crates/rekcod`: CLI plus the daemon that owns the VM (`objc2-virtualization`), the socket proxy (`proxy.rs`, rewrites `containers/create` for `dev.rekcod.local`), the port forwarder (published ports and `rekcod x` listeners), file events, the DNS bridge and clock sync. `snap.rs` and `xrun.rs` implement `rekcod snap` and `rekcod x`; `actions.rs` is the action list that `menubar.rs` renders.
- `crates/rekcod-agent`: static musl guest agent.
- `crates/rekcod-proto`: vsock ports and wire formats.
- `guest/`: a 6.12 LTS kernel built from Alpine's `linux-virt` config plus `guest/kernel/rekcod.config` (virtio, ext4, fuse and docker's netfilter built in, no initramfs; [docs/custom-kernel.md](docs/custom-kernel.md)), an Alpine ext4 root with upstream static docker binaries, and busybox init. `--build-arg KERNEL=alpine` builds the stock-kernel guest instead.
- `guest/fuse/`: the fuse/virtiofs patch (long TTLs, lazy open, host-driven invalidation), applied in-tree to the guest kernel.
- State lives in `~/.rekcod`: `vm/root.img` (replaced on upgrade), `vm/data.img` (256 GiB sparse by default, all docker data), `vm/warm-paths` (bind sources warmed at boot) and `logs/`. The sandbox VM has its own copy under `~/.rekcod/sandbox`.

Design and roadmap: [`docs/mvp.md`](docs/mvp.md), [`docs/plan.md`](docs/plan.md), [`docs/phase2.md`](docs/phase2.md), [`docs/phase2b-4.md`](docs/phase2b-4.md), [`docs/research.md`](docs/research.md), [`docs/phase5.md`](docs/phase5.md), [`docs/sandbox.md`](docs/sandbox.md), [`docs/frontends.md`](docs/frontends.md), [`docs/paas.md`](docs/paas.md), [`docs/agent-sandboxes.md`](docs/agent-sandboxes.md), [`docs/platform.md`](docs/platform.md). Next steps: [`docs/handoff.md`](docs/handoff.md).

## Limitations

- First access can be slower than OrbStack for a tree rekcod hasn't seen since boot, when a container touches it before background warming finishes (about 1 s per 20k files). Recently used trees are warmed at boot (a 19.5k-file walk 3 s after `rekcod start`: 34 ms warmed vs 286 ms not). Repeat work is 3–9× faster.
- Mac→container change latency is about 14 ms p50 (`bench/inotify_rtt.py`; OrbStack measured 12 ms).
- Memory isn't returned to macOS. `REKCOD_BALLOON=1` drives the balloon, but on macOS 27 the VM's footprint doesn't drop, even under memory pressure (docs/phase2.md, docs/phase5.md).
- `--sandbox` and `rekcod sandbox` haven't had an external security review (docs/sandbox.md).
- A sandbox fork copies the disk, not memory: processes restart in the child (Virtualization.framework can't restore a VM with a virtio-fs share).
- No brew formula. Releases are ad-hoc signed, not notarized.

Debug knobs:

- `REKCOD_DEBUG=1` logs forwarded file events.
- `REKCOD_KERNEL_CMDLINE` overrides the guest command line.
- `REKCOD_NO_BALLOON=1` drops the balloon device.
- In the guest, `cat /sys/module/virtiofs/parameters/rekcod_stats` shows host requests by FUSE opcode.

## License

Apache-2.0, see [`LICENSE`](LICENSE).
