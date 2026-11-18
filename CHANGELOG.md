# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

rekcod is pre-1.0. Until 1.0, a minor bump (0.2 to 0.3) may break CLI flags, on-disk state
in `~/.rekcod` or the fleet's API and database, and the entry says how to move over. A patch
bump (0.2.0 to 0.2.1) doesn't. The guest image isn't a public interface: every release pins
its own and downloads it on first start.

The release workflow publishes the section whose heading names the tag's version as the
release notes, and refuses to publish without one.

## [0.3.0] - Unreleased

### Upgrading from 0.2

- **Fleet controllers serve TLS.** Start the new controller, then re-add remotes and re-join
  nodes with the CA fingerprint `rekcod controller ca` prints on the controller:

  ```sh
  rekcod remote add NAME https://HOST:7700 --token-file - --ca-fingerprint sha256:... < admin-token
  REKCOD_JOIN_TOKEN="$(cat join-token)" rekcod node join https://HOST:7700 --ca-fingerprint sha256:... --install
  ```

  `controller serve --http` keeps plain HTTP on loopback and tailnet addresses. Upgrade the
  controller first: it accepts nodes and edges one minor version behind it, and CLIs must be
  on its minor version.
- **New nodes need `rekcod node approve ID`** unless the controller runs with
  `--auto-approve` (`server setup` passes it). Existing nodes stay ready. Nodes that join
  after the upgrade get node-named certificates; existing nodes keep theirs until they join
  again.
- **Tokens are hashed** on the controller's first start. They keep working; the plaintext
  is no longer in `fleet.db`, so keep `controller/admin-token`.
- **Edges should use an edge token** (`controller token edge --edge`) instead of an admin
  one. Admin tokens still work.
- **Each app has its own webhook.** `/webhooks/github` is gone. Run
  `rekcod app webhook NAME` and set each repository's hook up again with that URL and secret.
- **`rekcod.toml` is parsed as TOML**, and unknown sections and keys, wrong types and memory
  that isn't whole GiB fail the deploy. Check a file that worked with the old line parser.
  `app deploy` sends the committed `rekcod.toml`, not the working copy.
- **RustFS on a LAN address**: a server set up with `--addr` on a LAN address had RustFS
  there; `storage serve` and the controller now refuse to start with it. Run
  `rekcod server setup` again with the same options and redeploy bucket apps.
- **`rekcod mcp` and `rekcod sandbox serve` only create and reach sandboxes for the
  directory they started in** (`--root` adds others) and refuse `allow_hosts`, `secrets`,
  `mount`, macOS guests and `apply` unless started with `--allow-host`, `--secret`,
  `--allow-mount`, `--allow-macos` and `--allow-apply`. A sandbox with more than the flags
  allow isn't listed and every call on it gets a 403 naming the flag. A fleet node's API
  reaches only fleet sandboxes.
- **`sandbox apply --reviewed` takes the review ID** `sandbox diff` prints
  (`--reviewed REVIEW`). Through the API and MCP, `reviewed` is that ID as a string; a
  boolean is an error.
- **Sandbox API limits**: reading a file over 8 MiB is refused (use `rekcod sandbox cp`);
  flags must be JSON booleans or `"true"`/`"false"`, numbers JSON numbers or digits.
- **Nested dependency directories** (`node_modules`, `.venv`, `target` below the root) are
  diffed and applied unless the project already has that directory there.
- **`sandbox create` on another volume than `~/.rekcod`** needs `--mount`.
- Sandboxes from before this keep working; `apply` falls back to the old mtime conflict
  check for them, and compares modes only for sandboxes whose manifest records them.
- **Snapshot pushes have a new layout** (`snaps/<project>/<name>/<generation>/`). `snap
  restore --pull` still reads older pushes, but a rekcod from before this can't pull what this
  one pushes, so upgrade every Mac that pulls.
- **`uninstall --keep-data` keeps agent sandboxes.** It used to delete them.
- **The dashboard is on `http://127.0.0.1:PORT`** and has no cookie: a second tab picks the
  token up from `localStorage`. Bookmarks to `localhost:PORT` are redirected.
- `app destroy` waits for a running deploy of that app. App names of exactly 32 hex digits
  are refused.
- A data disk without the fresh marker is never formatted; `start --repair` runs `e2fsck`
  against the backup superblocks. A custom `REKCOD_HOME` must be absolute and is set to 0700.
- Containers created before this keep unlimited `json-file` logs until they're recreated.

### Security

- `install.sh` and `rekcod upgrade` verify build provenance with `gh attestation verify`
  when `gh` is installed, require the same Developer ID team over a Developer ID-signed
  rekcod, download over https only and say when only the checksum was checked. Releases
  carry `rekcod.sigstore.json`.
- A Bonjour answer or a stored host can never become an `ssh` option; a LAN server's host key
  is pinned at `host add` and `host refresh` follows only a Mac that holds it.
- `host add`, `host uninstall` and `uninstall` keep what they don't own in `~/.ssh/config`,
  `~/.zshenv` and `authorized_keys`, and write through symlinks.
- Another Mac's `rekcod state` can't type commands into a Terminal here: commands run from a
  private script file, and a host's state is checked before use.
- Agent sandboxes: every port and every HTTP request is checked against the allowlist
  (absolute-form, pipelined and bare-LF requests included); ports other than 80 and 443
  carry only TLS or HTTP unless the entry names the port. The sandbox, not its code, decides
  which Mac ports its daemon listens on. The guest agent takes vsock connections from the
  Mac only.
- `apply --reviewed` is tied to the review ID, and live shares bind every `.git`
  (worktree and submodule files, nested repositories) read-only.
- `host.docker.internal` traffic from other VMs on the NAT segment no longer reaches Mac
  loopback services. Port forwards stay off the dashboard's port, `[::1]` included.
- The fleet: the CA pin covers the whole `/ca.pem`, a job env entry without `=` is refused,
  app secrets, S3 keys and tokens stay off argv and out of plists, RustFS listens only where
  asked, fork pull requests deploy only the labelled commit, and edge tokens can't ban
  wider than a /24 (/48) for more than a day.
- The guest kernel has KASLR, `FORTIFY_SOURCE` and slab freelist hardening on, and kexec,
  hibernation, debugfs and `KALLSYMS_ALL` off.
- The dashboard sends a Content-Security-Policy.

### Added

- Elastic memory (macOS 27): `--memory` is split into base RAM and swap backed by memory
  rekcod hands back to macOS once the guest frees it, so a burst no longer leaves its memory
  with the VM until `rekcod stop`. `rekcod start --base-memory N` sets the split (default:
  half, at least 4 GiB); `rekcod status` shows how much elastic memory the Mac holds now.
- The menu bar and dashboard manage images (pull, run, tag, push, layers), containers
  (pause, resume, shell), volumes (create, files, clone, export), networks and
  `system prune`. Actions that need a value ask for it, and both front ends check it.
- `rekcod host state NAME` and `rekcod host exec NAME -- ARGS`: another Mac's rekcod, with
  its actions rewritten to run there. The menu bar gets an Engine submenu per host; the
  dashboard switches engines from the sidebar.
- `rekcod upgrade` restarts the controller, node, edge and storage services that run the
  binary, and keeps the old binary as `.rekcod.previous`; `rekcod upgrade --rollback` and the
  "Go back to the previous version" action put it back.
- `rekcod sandbox prune [--older-than 168h]` and "Remove unused agent sandboxes".
- `rekcod sandbox apply --check` exits 0, 1 or 2; `rekcod doctor --json`; doctor checks the
  guest version.
- Allowlist entries with a port (`db.example.com:5432`).
- `x --sandbox -p` and `--no-autoports` limit what the sandbox VM forwards.
- `rekcod socket install` also installs a root LaunchDaemon that restores the
  `/var/run/docker.sock` route at every boot.
- Fleet: per-app webhooks, `controller tokens`, `controller audit`, `controller restore
  --from FILE`, `--backup-dir` copies, node certificate renewal and expiry warnings, tokens
  from a file, stdin or the environment, per-app `[http] timeout`, edge connection caps and
  graceful shutdown.
- Durations take days (`--expires 30d`, `idle_stop = "1d"`).
- `rekcod host uninstall NAME` (menu bar and dashboard: Remove rekcod from NAME): `rekcod uninstall`
  on that Mac, rekcod's key out of its `authorized_keys`, then its docker context, SSH config and
  remote here. `rekcod uninstall` also removes the `~/.zshenv` block, firewall exceptions and
  puts `pmset` back as `server setup` found it.
- `rekcod host restart NAME` (menu bar and dashboard: Restart NAME) and `rekcod server restart`:
  reboot a server so it comes back on its own, FileVault unlocked for that one boot. One
  password prompt, checked before anything restarts; `host restart` waits until it's back.
- `server setup` installs the services and the docker VM as LaunchDaemons that run as you and
  start at boot with nobody logged in, whenever it can sudo; `rekcod server at-boot` moves a
  setup done without sudo (LaunchAgents) over, and the pkg does that as root.
- `host add` on a Mac that only takes an SSH password creates `~/.rekcod/ssh/id_ed25519`, installs
  it with `ssh-copy-id` (one prompt) and includes `~/.rekcod/ssh/config` from `~/.ssh/config`, so
  docker's ssh gets in too. `host rm` and `host refresh` keep the config in step; `uninstall`
  removes the Include.
- `host add --server` from a dev build copies the local guest image to the server.
- `scripts/package-server.sh` builds `rekcod-server-X.Y.Z.pkg`: AirDrop it to a Mac and
  double-click. It installs rekcod and the guest, runs `server setup` for the logged-in user and
  leaves the `rekcod remote add` line in `~/Desktop/rekcod-pair.txt`.
- `rekcod snap save|restore|ls|rm`: compose volume snapshots, named after the git branch by
  default.
- `rekcod x`: run a command in a container picked from the project's runtime, with VM-local
  dependency directories and listeners forwarded to the Mac. `--sandbox` runs it in a second
  VM that sees only the project directory.
- `dev.rekcod.local` container label: the socket proxy puts those paths on VM-local volumes.
- `rekcod menubar`: menu bar item with the same actions as the CLI.
- `rekcod socket install|uninstall`: route `/var/run/docker.sock` to rekcod while it runs.
- Custom guest kernel built from kernel.org sources with no initramfs (agent up in 0.39 s,
  was 0.55 s). `KERNEL=alpine` builds Alpine's `linux-virt` of the same version.
- Published TCP ports connect straight to the container instead of through docker-proxy.
- `rekcod sandbox`: many throwaway VMs, each with a copy of one project. Fork, diff, apply,
  golden disks, a DNS egress allowlist, an HTTP API on a unix socket, and `rekcod mcp`.
- `rekcod controller|node|edge` and `rekcod app|job`: a self-hosted fleet that places
  sandboxes on Macs, deploys apps from git with health-gated rollouts, PR previews, scale to
  zero, recycle and scheduled jobs. `--install` runs each part as a LaunchAgent. The
  controller keeps its state in SQLite (`~/.rekcod/controller/fleet.db`).
- `rekcod macos`: macOS sandboxes from Apple's restore image.
- `rekcod host add ... --server`: provisions another Mac as a one-machine fleet over SSH.
- `rekcod storage`: RustFS for per-app buckets, stored images, fleet database backups and
  `snap save --push`. Edge rate limits, bans and per-app HTTP settings.
- `rekcod state` and `rekcod dashboard`: a web dashboard (Next.js) over the menu bar's
  action lists.
- `scripts/install.sh`: verified install from a release.
- Releases carry CycloneDX SBOMs and a build provenance attestation.
- `rekcod start --repair` (e2fsck the data disk, fresh root disk), `start --install` (boot at
  login and again after a crash), `stop --force`. Start settings are saved and reused.
- `rekcod upgrade`, `rekcod uninstall`, `rekcod doctor`.
- `rekcod start --network auto|user|nat`: the main VM can use rekcod's userspace network
  (the one sandboxes use, unfiltered) so full-tunnel VPNs like Mullvad, which drop
  Virtualization.framework NAT traffic, don't cut the VM off. `auto` is the default.
- Fleet: a reconcile loop that takes back instances that answer again and replaces missing
  ones, `node approve|rm`, `controller ca|revoke|rotate-join`, `--alert-webhook`,
  `--backup-dir`, `GET /metrics`, kept logs of removed instances, graceful SIGTERM.
- `scripts/e2e-lifecycle.sh`: VM crash, damaged-disk and repair checks on a VM of its own.

### Changed

- Original rekcod code is now MIT licensed, including the guest drivers.
- The guest runs Linux 7.3 on Alpine 3.24 with Docker 29.9.3; the Mac's Docker CLI is
  29.9.3, Compose 5.7.0 and Buildx 0.38.2. `bump.yml` moves the pins to the newest releases
  weekly and checks kernel.org and Alpine signatures. Sandboxes default to
  `debian:trixie-slim`, `rekcod x` to Python 3.14.
- dockerd caps each container's `json-file` log at 10 MB in 3 files.
- The dashboard and every link to it use `127.0.0.1`, never `localhost`.
- `rekcod uninstall` stops before deleting anything still running, names the directory it
  deletes, refuses a directory with files rekcod didn't put there, and under another
  `REKCOD_HOME` removes only that home.
- `sandbox diff` is rendered by rekcod, not git: the workspace's git config and
  `.gitattributes` no longer run.
- Downloads give up after 15 s without a connection or 60 s below 1 KiB/s, with no cap on
  the whole download (the old cap on `upgrade` was 60 s).
- `--install` makes relative path flags absolute, refuses a job another `REKCOD_HOME`
  installed, and copies `REKCOD_*`, `DOCKER_CONFIG` and `S3_*` settings into the job.
- `console.log`, `dashboard.log`, `storage.log` and the fleet logs are trimmed at 20 MB while
  their writers run.
- `start --repair` waits up to 30 minutes; e2fsck time doesn't count against the boot wait.
- `stop --force` and `sandbox rm` kill a daemon still there 45 s after asking it to stop.
- `start --no-switch` with a non-default `REKCOD_HOME` leaves the shared docker context alone.
- Snapshots are written as a new generation before the old one goes; restore saves the
  current volumes as `pre-restore` first.
- DNS uses per-domain resolvers from `scutil --dns` (VPN split DNS, Tailscale).
- `daemon.log` and `console.log` start fresh each boot, keeping the previous as `.1`.
- Releases bundle the dashboard and run fmt, clippy, all tests, `cargo deny`, the dashboard
  checks and a QEMU boot of the guest before publishing, and the VM suites on a self-hosted
  Mac when one is set up. Workflow actions are pinned by commit SHA, and the Rust toolchain
  by `rust-toolchain.toml`. The same inputs build the same guest.
- The guest build checks a pinned sha256 for every download and pins the Alpine release and
  kernel version (`guest/versions.env`).
- Sandbox VMs no longer use VZ NAT. Their network card is a socket the daemon serves with a
  userspace TCP/IP stack (smoltcp), and the daemon checks every connection: Mac and LAN
  refused, allowlisted sandboxes limited to addresses their allowed names resolved to, with
  the TLS SNI or HTTP Host checked. Guest root flushing iptables no longer opens anything.
  `--offline` sandboxes get no network card.
- Sandbox DNS forwards only standard one-question queries.

### Fixed

- `[http] timeout` in `rekcod.toml` is accepted.
- `rekcod import --help` prints rekcod's help instead of booting the VM.
- `--memory` is checked before it's saved, and a failed boot puts the old settings back.
- A renamed container keeps its published port.
- The `/var/run/docker.sock` route comes back after a reboot.
- A sandbox with a long exec or an open shell isn't powered off by its idle TTL.
- `rekcod x --sandbox` stops its VM only after its last user.
- Volume exports never overwrite an earlier one.
- `host add` reuses one SSH connection (a dozen handshakes took most of a minute over Wi-Fi) and
  prints each step, including that sudo on the server is about to ask for a password.
- `server setup` on a Mac without Homebrew's xz: RustFS 1.0.1 loads macOS's own `liblzma`
  instead.
- `server setup` used a stopped Tailscale's old IP; it now checks the IP is on an interface.
- `server setup` with the application firewall on lets rekcod accept connections (sudo), or
  prints the command.
- `server setup` re-runs: waits for launchd to drop the old agent before `bootstrap`, waits for
  the restarted controller to listen, and trusts the controller's CA before asking it for the
  edge token.
- `host add --server` found no rekcod and no `$HOME` on the server (arguments split over SSH),
  never replaced an older dev build, could send a binary without the virtualization
  entitlement, and left the server without a docker CLI.
- `host add --server` copies rekcod to the server beside the old binary and renames it over,
  instead of overwriting the file a running agent was started from.
- Readings nobody could take are `null` and show as `–`, not 0: node CPUs, load and disk,
  the guest's disk usage, VM CPUs/memory in `rekcod status`, `docker stats` rows that print
  `--`. The controller's `nodes` table gets nullable columns (migration 5).
- The menu bar shows "Loading…" until its first read, and a failed docker list shows its error
  (`local_errors` in `rekcod state`) instead of "No containers".
- Dashboard charts no longer draw lines across time with no samples, fill idle traffic minutes
  with 0 requests, and the file-sharing rate no longer dips to 0 on cached polls.
- A data disk that isn't readable ext4 is never formatted (it was, on any blkid miss); its
  backup superblock is tried and the error reported.
- A reader that falls behind a vsock stream no longer gets a short stream with a clean EOF.
- `snap save --push` uploads into a new generation and writes the manifest last, so an
  interrupted push no longer mixes archives from two pushes; `--pull` checks each archive's
  SHA-256 and size.
- `snap restore --pull` of a backup older than the local snapshot restores the backup, and of
  two saves in the same second the second is kept. Saves, restores and removals of one
  project take turns.
- A compose volume named `done` no longer makes `snap save` delete the snapshot it reports
  saved, and a save whose generation didn't commit fails instead of reporting success.
- A `docker pause` that fails partway through `snap save` no longer leaves the containers it
  already paused frozen.
- `import --from` a context that reaches rekcod's own daemon is refused instead of emptying
  the volumes it was about to copy, and import exits non-zero when anything failed.
- S3 downloads that end early are errors, not short files. Keys containing entity text such
  as `&amp;` list and delete as themselves. A bucket check that gets a 500 is an error, not
  "no bucket", so a bucket delete that didn't happen isn't reported as done.
- A stale pid file no longer counts as a running VM, and `stop` no longer signals whatever
  process reused the pid. Two commands starting the VM at once boot one VM.
- Mac ports another process serves on `*:PORT` aren't shadowed by a `127.0.0.1:PORT`
  forward.
- File-change batches the guest didn't take are resent instead of dropped.
- The menu bar no longer freezes on a slow controller or a slow host.
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

[0.3.0]: https://github.com/TimMikeladze/rekcod/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/TimMikeladze/rekcod/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/TimMikeladze/rekcod/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/TimMikeladze/rekcod/releases/tag/v0.1.0
