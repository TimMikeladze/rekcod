# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

rekcod is pre-1.0. Until 1.0, a minor bump (0.2 to 0.3) may break CLI flags, on-disk state
in `~/.rekcod` or the fleet's API and database, and the entry says how to move over. A patch
bump (0.2.0 to 0.2.1) doesn't. The guest image isn't a public interface: every release pins
its own and downloads it on first start.

## [Unreleased]

Version 0.3.0 in `Cargo.toml`; not tagged yet.

### Upgrading from 0.2

- **Fleet controllers serve TLS.** Start the new controller, then re-add remotes with
  `rekcod remote add NAME https://HOST:7700 --token T --ca-fingerprint $(rekcod controller ca)`
  and re-join nodes with `--ca-fingerprint`. `controller serve --http` keeps plain HTTP on
  loopback and tailnet addresses. Controllers, nodes and CLIs must be on the same minor
  version; older ones get an error naming both versions.
- **New nodes need `rekcod node approve ID`** unless the controller runs with
  `--auto-approve` (`spare setup` passes it). Existing nodes stay ready.
- **Tokens are hashed** on the controller's first start. They keep working; the plaintext
  is no longer in fleet.db, so keep `controller/admin-token`.
- **Edges should use an edge token** (`controller token edge --edge`) instead of an admin
  one. Admin tokens still work.
- **`rekcod mcp` and `rekcod sandbox serve` only create sandboxes for the directory they
  started in** (`--root` adds others) and refuse `allow_hosts`, `secrets` and `mount` unless
  started with `--allow-host`, `--secret` and `--allow-mount`.
- Sandboxes from before this keep working; `apply` falls back to the old mtime conflict
  check for them.

### Security

- Git URLs and refs that git would read as options (`--upload-pack=...`) are refused; only
  https, ssh and http transports (local repos with `REKCOD_ALLOW_LOCAL_GIT=1`).
- The node API's sandbox id can't be replaced by a query parameter; remote sandbox creates
  forward only user fields; golden disk and macOS image names are validated.
- `sandbox apply` never writes through a symlink, refuses symlinks out of the project and
  applies a frozen copy (`--reviewed` for what `diff` showed).
- The dashboard needs a per-launch token, and containers can't reach its port through
  `host.docker.internal`.
- Workspace containers drop `NET_RAW`; a live `.git` is mounted read-only into sandboxes.
- Fleet: TLS with a pinned CA, hashed and revocable tokens with expiry, edge tokens, node
  approval and join-token rotation, presigned URLs instead of RustFS root keys on nodes,
  viewers can't read env values, webhook replay protection, fork PRs need a label, request
  size and connection limits, constant-time token checks.

### Added

- `rekcod snap save|restore|ls|rm`: compose volume snapshots, named after the git branch by
  default.
- `rekcod x`: run a command in a container picked from the project's runtime, with VM-local
  dependency directories and listeners forwarded to the Mac. `--sandbox` runs it in a second
  VM that sees only the project directory.
- `dev.rekcod.local` container label: the socket proxy puts those paths on VM-local volumes.
- `rekcod menubar`: menu bar item with the same actions as the CLI.
- `rekcod socket install|uninstall`: route `/var/run/docker.sock` to rekcod while it runs.
- Custom guest kernel built from kernel.org sources with no initramfs (agent up in 0.39 s,
  was 0.55 s). `KERNEL=alpine` builds the stock one.
- Published TCP ports connect straight to the container instead of through docker-proxy.
- `rekcod sandbox`: many throwaway VMs, each with a copy of one project. Fork, diff, apply,
  golden disks, a DNS egress allowlist, an HTTP API on a unix socket, and `rekcod mcp`.
- `rekcod controller|node|edge` and `rekcod app|job`: a self-hosted fleet that places
  sandboxes on Macs, deploys apps from git with health-gated rollouts, PR previews, scale to
  zero, recycle and scheduled jobs. `--install` runs each part as a LaunchAgent. The
  controller keeps its state in SQLite (`~/.rekcod/controller/fleet.db`).
- `rekcod macos`: macOS sandboxes from Apple's restore image.
- `rekcod host add ... --fleet`: provisions a spare Mac as a one-machine fleet over SSH.
- `rekcod storage`: RustFS for per-app buckets, stored images, fleet database backups and
  `snap save --push`. Edge rate limits, bans and per-app HTTP settings.
- `rekcod state` and `rekcod dashboard`: a web dashboard (Next.js) over the menu bar's
  action lists.
- `scripts/install.sh`: checksum-verified install from a release.
- Releases carry CycloneDX SBOMs and a build provenance attestation.

- `rekcod start --repair` (e2fsck the data disk, fresh root disk), `start --install` (boot at
  login and again after a crash), `stop --force`. Start settings are saved and reused.
- `rekcod upgrade`, `rekcod uninstall`, `rekcod doctor`.
- Fleet: a reconcile loop that takes back instances that answer again and replaces missing
  ones, `node approve|rm`, `controller ca|revoke|rotate-join`, `--alert-webhook`,
  `--backup-dir`, `GET /metrics`, kept logs of removed instances, graceful SIGTERM.
- `scripts/e2e-lifecycle.sh`: VM crash, damaged-disk and repair checks on a VM of its own.

### Changed

- `start --no-switch` with a non-default `REKCOD_HOME` leaves the shared docker context alone.
- Snapshots are written as a new generation before the old one goes; restore saves the
  current volumes as `pre-restore` first.
- DNS uses per-domain resolvers from `scutil --dns` (VPN split DNS, Tailscale).
- `daemon.log` and `console.log` start fresh each boot, keeping the previous as `.1`.
- Releases bundle the dashboard and run fmt, clippy, all tests, `cargo deny` and the
  dashboard checks before publishing. Workflow actions are pinned by commit SHA, and the
  Rust toolchain by `rust-toolchain.toml`.
- The guest build checks a pinned sha256 for every download and pins the Alpine release and
  `linux-virt` version (`guest/versions.env`).

### Fixed

- A data disk that isn't readable ext4 is never formatted (it was, on any blkid miss); its
  backup superblock is tried and the error reported.
- A reader that falls behind a vsock stream no longer gets a short stream with a clean EOF.
- A stale pid file no longer counts as a running VM, and `stop` no longer signals whatever
  process reused the pid. Two commands starting the VM at once boot one VM.
- Mac ports another process serves on `*:PORT` aren't shadowed by a `127.0.0.1:PORT`
  forward.
- File-change batches the guest didn't take are resent instead of dropped.
- The menu bar no longer freezes on a slow controller.
- Fleet: lost nodes' instances come back, nodes boot their instances after a reboot,
  concurrent deploys of one app are serialized, a restart cleans up half-done deploys, jobs
  a node lost are reported, `--listen 0.0.0.0 --advertise ...` produces reachable endpoints.
- `rekcod x --sandbox` exits as soon as the command does.
- `scripts/e2e.sh` removes only its own `rekcod x` container and refuses its final
  `rekcod stop` while containers it didn't start are running.

## [0.2.1] - 2026-10-12

### Fixed

- Database-style fsync performance: VM disks use fsync instead of a full media flush per
  guest flush (pgbench 979 to 15.2k TPS). `REKCOD_DISK_SYNC=full` restores the old mode.
- vCPUs default to the Mac's performance-core count (at least 4) instead of every core.

## [0.2.0] - 2026-10-12

### Added

- `rekcod <docker command>` runs docker against rekcod, booting the VM if needed. Without a
  docker CLI on `PATH`, a sha256-pinned docker, compose and buildx are fetched on first use.
- A versionless release asset, so `releases/latest/download/rekcod-darwin-arm64.tar.gz` works.
- Apache-2.0 license.

### Changed

- The data disk is 256 GiB sparse by default and grows in place with `--disk`. The BuildKit
  cache is capped at 20 GB.
- `rekcod import` copies volumes with `docker cp` and checks entry count and bytes.

## [0.1.0] - 2026-10-11

First release.

- Upstream dockerd 29.9.0 in a Virtualization.framework VM, all host and guest traffic over
  vsock. `rekcod start|stop` switch and restore the docker context; `rekcod import` copies
  images and volumes from another context.
- Published TCP and UDP ports on the Mac, DNS through the Mac's resolvers,
  `host.docker.internal` (including Mac loopback), Rosetta for `linux/amd64`.
- Bind mounts through a patched fuse/virtiofs with FSEvents-driven invalidation.
- `rekcod compact`, hourly trim and bind-mount warming.
- Release tarballs with `SHA256SUMS`; `rekcod start` downloads and verifies the guest the
  binary was built with.

[Unreleased]: https://github.com/TimMikeladze/rekcod/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/TimMikeladze/rekcod/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/TimMikeladze/rekcod/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/TimMikeladze/rekcod/releases/tag/v0.1.0
