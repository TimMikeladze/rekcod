# rekcod

rekcod is an open source Docker Desktop alternative for Apple Silicon Macs. Keep your
Docker commands and Compose files, with [3–9× faster warm bind-mount operations](#read-your-source-tree-faster)
in the recorded M5 Pro benchmark.

Save your database after seeding and restore it before the next test. Give a coding
agent its own VM and review its changes before they reach your project.

## Run your first container

On an Apple Silicon Mac:

```sh
curl -fsSL https://raw.githubusercontent.com/TimMikeladze/rekcod/main/scripts/install.sh | bash
rekcod start
rekcod run --rm hello-world
```

The installer verifies the release checksum. The first start downloads the Linux guest
image and boots Docker. If you don't have a Docker CLI, rekcod downloads Docker, Compose
and Buildx for you.

With an existing Docker CLI, `rekcod start` selects the `rekcod` context.
`rekcod stop` powers off the VM and restores your previous context. You can
[copy images and volumes from Docker Desktop or OrbStack](#bring-your-images-and-volumes)
when you're ready.

### Bring your images and volumes

To keep existing database data, import before starting your Compose project on rekcod.
Leave the source engine running, with its project containers stopped. Choose the command
for your engine:

```sh
rekcod import --from desktop-linux   # Docker Desktop
rekcod import --from orbstack        # OrbStack
```

Import copies images and named volumes, leaving the originals at the source. Volumes
still in use are skipped. Use `--images-only` or `--volumes-only` to limit the copy.
[Migration walkthrough](docs/getting-started.md).

## Run the project you already have

From your Compose project directory:

```sh
rekcod compose up -d
rekcod compose logs -f
rekcod ps
```

You can also keep using `docker compose up -d` while the rekcod context is active.
Docker commands passed directly to `rekcod` always use its local engine and boot it if
needed.

Published ports appear on the Mac's `localhost`. Edits to bind-mounted source trigger
inotify inside containers, so your existing hot-reload setup works.
`host.docker.internal` reaches services on your Mac, and container DNS follows the
Mac's resolvers, including domain-specific VPN resolvers.

rekcod runs the unmodified upstream `dockerd` with its embedded BuildKit. Builds,
Compose and Docker API clients use the engine itself, including buildx workflows.

## Advantages

- Keep your Docker setup: upstream `dockerd`, BuildKit, Compose and Docker API clients.
- Run the CLI, VM daemon and menu bar from one native executable, with an optional web UI.
- Repeat source-tree scans and reads from the guest cache, with recorded warm gains of 3–9×.
- Reuse seeded databases through volume snapshots and keep Linux toolchains off your Mac.
- Review an agent's work in a project copy before applying its changes.

## Binary and download sizes

The CLI, daemon and menu bar share a native arm64 Mach-O executable. The v0.3.0 source
build measured **11.29 MiB** with `cargo build --release --locked -p rekcod`.

That executable manages the VM from macOS. Linux containers need a Linux kernel, so
Docker runs in a separate Linux VM called the **guest**. Its image contains the kernel,
a minimal Alpine filesystem, upstream Docker with containerd and runc, and the Linux
rekcod agent. The guest is downloaded once per guest version and cached for later starts.

| Host artifact | Version | Size |
| --- | --- | --- |
| rekcod native executable | 0.3.0 source build | 11.29 MiB |
| OrbStack installed application bundle | 2.2.3 | 694.89 MiB |
| Docker Desktop installed application bundle | 4.88.0 | 2,192.70 MiB |

Measured on 2026-11-08. Application bundles were measured with `du -sk`; they contain
more components than an executable. rekcod downloads its Linux guest separately.
Docker CLI plugins, the optional dashboard and its Node.js runtime add to its footprint.

For compressed downloads on Apple Silicon:

| Download | Version | Size |
| --- | --- | --- |
| rekcod CLI archive | 0.2.1 | 0.93 MiB |
| rekcod Linux guest archive | 0.2.1 | 99.38 MiB |
| Docker Desktop DMG | 4.98.0 | 546.12 MiB |

For v0.2.1, the compressed downloads total **100.30 MiB**: **0.93 MiB** for the CLI
archive plus **99.38 MiB** for the guest archive, before optional CLI tools or UI.
The **11.29 MiB** figure above is the newer v0.3.0 host executable, uncompressed; it
excludes the guest. A complete v0.3.0 download total has not been measured here.
[rekcod release assets](https://github.com/TimMikeladze/rekcod/releases/tag/v0.2.1) ·
[Docker Desktop arm64 DMG](https://desktop.docker.com/mac/main/arm64/245422/Docker.dmg).

Container images and volumes use additional storage in every engine. rekcod's default
256 GiB data disk is sparse: host storage is allocated as data is written.

## Startup, sandboxes and memory

These are recorded measurements with guest files and required images already available.
The linked results describe each test's configuration.

| Task | Result | Setup and source |
| --- | --- | --- |
| Start the Docker VM | 0.8 s median | 4 vCPU, 2 GiB RAM, five boots; [boot results](docs/custom-kernel.md#results) |
| Create an agent sandbox to first exec | 1.32 s p50 | Warm golden disk, M5 Pro; [sandbox results](docs/agent-sandboxes.md#as-built) |
| Fork an agent sandbox to first exec | 1.18 s p50 | M5 Pro; [sandbox results](docs/agent-sandboxes.md#as-built) |
| Create 16 sandboxes concurrently | 3.2 s | M5 Pro; [sandbox results](docs/agent-sandboxes.md#as-built) |

On macOS 27, elastic memory returns burst capacity while the VM keeps running. In the
recorded 5 GB allocation test, VM resident memory fell from 8,610 MB to 2,766 MB within
15 seconds after the container exited, using 2 GiB base RAM and 6 GiB elastic memory.

Base RAM stays with the VM until it stops. Memory above the base uses a swap path,
so sustained workloads should have enough base RAM for their working set.
[Memory measurements and sizing](docs/elastic-memory.md).

## Read your source tree faster

A test collector, compiler or file watcher can inspect thousands of files in a single
run. On a bind mount, each uncached lookup can cross from Linux to macOS. rekcod caches
file metadata and reads in the guest, then invalidates affected entries when you edit
files on the Mac.

In the recorded M5 Pro benchmark, warm bind-mount operations were 3–9× faster than
OrbStack:

| Operation | rekcod bind mount | OrbStack bind mount | Native macOS |
| --- | --- | --- | --- |
| List and stat files, warm | 34–47 ms | 142 ms | 110 ms |
| Look up missing files, warm | 89–112 ms | 668 ms | 232 ms |
| Open and read files, warm | 104–108 ms | 909 ms | 510 ms |
| First read of the tree, cold | ~1.7 s | 841 ms | 447 ms |
| Create and delete files | 1.5–1.8 s | 714 ms | 539 ms |

Measured on 2026-10-11, macOS 27.0.1, with a 20.5k-file tree. These are filesystem
benchmarks; whole-build time depends on how much work is file access.
[Method and results](docs/phase2.md).

The first access to a tree can be slower while the cache fills. rekcod warms
bind-mounted trees in the background and keeps the guest cache across container runs.

### Keep dependency writes inside Linux

For installs and build output, you can keep selected directories entirely in the VM.
Add a label to your Compose service:

```yaml
services:
  web:
    build: .
    working_dir: /app
    volumes:
      - .:/app
    labels:
      dev.rekcod.local: "node_modules,.next"
```

rekcod mounts named volumes at those paths when it creates the container. Dependency
installs and build output stay inside Linux, while your source remains editable on the
Mac. The volumes survive container recreation. Their contents are only visible inside
the VM, so Mac tools that need those dependencies need their own installation.

## Restore your database to a known state

Save a Compose project's named volumes after migrating and seeding. You can return to
that state after a test, a broken migration or an experiment:

```sh
rekcod snap save seeded
rekcod snap restore seeded
```

Run these from the Compose project directory. Saving pauses its running containers while
copying the volumes, then unpauses them. Restoring stops the running containers, copies
the saved data back and restarts them.

You can also name snapshots after your current git branch:

```sh
rekcod snap save
rekcod snap ls
```

Before replacing data, a restore saves the current volumes as `pre-restore`.
Run `rekcod snap restore pre-restore` to recover that previous state.
Snapshots live in the VM; `save --push` and `restore --pull` also transfer them through
[S3-compatible storage](docs/rustfs.md).

## Run project tools without installing them on your Mac

Use the project's Linux toolchain from its directory:

```sh
rekcod x bun install
rekcod x bun run dev
rekcod x uv run pytest
rekcod x cargo test
```

`rekcod x` picks an image from the command and project version files such as
`.bun-version`, `.nvmrc`, `.python-version` and `rust-toolchain.toml`. It mounts the
current directory at the same path and forwards listening ports to the Mac.

Linux dependencies and build output, including `node_modules`, `.venv` and `target`,
stay in VM volumes. Mac and Linux builds of native dependencies stay separate.
Use `--image` to choose an image yourself:

```sh
rekcod x --image alpine:3.22 sh
```

For a command that should only see the current project directory,
`rekcod x --sandbox bun install` runs in a separate VM. It shares that directory
read-write and blocks connections to the Mac and local network. The
[sandbox guide](docs/sandbox.md) covers network access and the isolation limits.

## Review an agent's changes before applying them

Give a coding agent a copy of your repository in its own VM. It can install packages,
run tests and use Docker there while your original project stays as it was.

For example, from a Bun project:

```sh
sandbox_id=$(rekcod sandbox create)
rekcod sandbox exec "$sandbox_id" -- bun install
rekcod sandbox exec "$sandbox_id" -- bun test
rekcod sandbox diff "$sandbox_id"
```

When you've reviewed the diff, apply that version and remove the sandbox:

```sh
rekcod sandbox apply "$sandbox_id" --reviewed
rekcod sandbox rm "$sandbox_id"
```

`apply --reviewed` writes the workspace version captured by the last `diff`.
It refuses to overwrite files you changed in the original project since they were copied
into the sandbox.

Linux agent sandboxes allow package registries and GitHub by default, with the Mac and
local network blocked. Additional destinations and secrets are explicit options.
Sandbox isolation hasn't had an external security review; see the
[agent sandbox guide](docs/agent-sandboxes.md) for its boundaries.

Agents can use the same operations through the `rekcod mcp` stdio server.
[Connect an MCP client](docs/agent-sandboxes.md#http-api-and-mcp).

## Manage it from the terminal, menu bar or dashboard

```sh
rekcod status
rekcod menubar --install   # open now and start at login
rekcod dashboard           # open the web UI; needs Node.js
rekcod doctor              # check setup and report fixes
rekcod --help
```

The menu bar and dashboard expose the same actions as the CLI: container controls,
logs, snapshots and sandbox changes. The dashboard uses `http://localhost:4500` or the
next free port.

`rekcod status` shows disk use and forwarded ports, including ports another Mac process
already occupies. VM sizing and startup settings are described in the
[getting started guide](docs/getting-started.md).
For setup problems, start with [troubleshooting](docs/troubleshooting.md).

### Docker SDKs and IDEs

The first `rekcod start` in a terminal may ask for your password to route
`/var/run/docker.sock`, so clients that ignore Docker contexts can connect.
Stopping rekcod restores the previous socket route.

If a shell already sets `DOCKER_HOST`, it overrides Docker contexts.
See [socket setup](docs/default-socket.md) for configuration details.

## How it works

The macOS daemon owns a Linux VM through Apple's Virtualization.framework. It forwards
Docker API requests over vsock to a guest agent, which connects to upstream `dockerd`.
The guest kernel's virtio-fs cache receives invalidations from macOS FSEvents, so file
edits also reach Linux file watchers.

State lives under `~/.rekcod`. Docker data is on `vm/data.img`, separate from the guest
root image replaced during upgrades. Agent sandboxes have their own disks and workspace
copies.

## Use the same tools beyond your laptop

| Task | Guide |
| --- | --- |
| Use another Mac as your Docker engine over SSH | [Remote Docker](docs/remote-docker.md) |
| Host apps on a Mac with deployments and rollbacks | [Self-hosting](docs/self-hosting.md) |
| Run local S3-compatible storage | [RustFS](docs/rustfs.md) |
| Run a self-hosted GitHub Actions runner in the VM | [Runner example](examples/github-runner/README.md) |

[All docs](docs/README.md) · [Build and contribute](CONTRIBUTING.md) · [MIT](LICENSE)
