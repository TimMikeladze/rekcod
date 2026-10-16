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
curl -fsSL https://raw.githubusercontent.com/TimMikeladze/rekcod/main/scripts/install.sh | bash
rekcod start      # first run downloads and verifies the guest image (~100 MB)
```

[`scripts/install.sh`](scripts/install.sh) downloads `rekcod-darwin-arm64.tar.gz` and
`SHA256SUMS`, checks the tarball's sha256 and installs nothing on a mismatch. Then it puts
`rekcod` in `/usr/local/bin` and, when the release has it, the dashboard in
`/usr/local/share/rekcod/dashboard`, asking for sudo only if `/usr/local` isn't writable.
`REKCOD_VERSION=0.2.1` installs that release, `PREFIX` installs elsewhere.

By hand, the same steps:

```sh
base=https://github.com/TimMikeladze/rekcod/releases/latest/download
curl -fsSLO "$base/rekcod-darwin-arm64.tar.gz" -O "$base/SHA256SUMS"
shasum -a 256 -c SHA256SUMS --ignore-missing     # must print "rekcod-darwin-arm64.tar.gz: OK"
tar -xzf rekcod-darwin-arm64.tar.gz
sudo mkdir -p /usr/local/bin && sudo mv bin/rekcod /usr/local/bin/
xattr -d com.apple.quarantine /usr/local/bin/rekcod 2>/dev/null   # not notarized yet
```

The binary pins the guest it was released with (`~/.rekcod/guest/<version>/`) and
checks its sha256 before unpacking. `REKCOD_RELEASE_URL` points the download elsewhere.

### Upgrading, removing, when something's wrong

```sh
rekcod upgrade             # latest release over this binary, checksum-verified; --check only looks
rekcod stop && rekcod start   # the VM keeps running the old version until it restarts
rekcod doctor              # checks the VM, versions, disk, sockets, context, VM internet; --bundle tars the logs
rekcod uninstall           # stops it, removes login agents, socket route, docker context and ~/.rekcod
rekcod uninstall --keep-data   # the same, keeping ~/.rekcod
```

`upgrade` refuses on a Homebrew install (`brew upgrade rekcod`; the formula is in
`packaging/homebrew`). `uninstall` asks first and leaves the binary for you to delete.
Details: [`docs/install-upgrade.md`](docs/install-upgrade.md).

## What works

Verified by `scripts/e2e.sh` (47 checks) and `tests/stress/` on an M5 Pro, macOS 27:

- Upstream dockerd 29.9.0 with its embedded BuildKit. `docker build`, compose `build:` and `buildx bake` all work.
- Published TCP and UDP ports on the Mac's `localhost`, mirrored from docker events. If a Mac port is already taken (even by a process on `*:PORT` while the container asks for `127.0.0.1:PORT`), rekcod reports it in `rekcod status` and leaves the other process alone. Mac TCP connections go straight to the container's address, skipping docker-proxy (about 85 µs round trip vs 125 µs through it), so iptables rules in the guest's `DOCKER-USER` chain don't apply to them.
- Bind mounts from `/Users`, `/Volumes`, `/private`, `/tmp` and `/var/folders` at the same paths, cached in the
  guest and invalidated from FSEvents. Warm work is 3–9× faster than OrbStack's bind
  mounts and faster than native macOS (table below). Mac-side edits, atomic saves, creates,
  deletes and chmods reach containers within about 14 ms and fire inotify, so hot reload works.
  Case-insensitive names behave like on the Mac (`Foo` is visible as `foo`). A bind-mounted
  tree is pre-warmed in the background from the moment its container is created, and the
  most recently used ones are warmed again right after `rekcod start`.
- `linux/amd64` images through Rosetta.
- `host.docker.internal` (including Mac services bound only to `127.0.0.1`) and `gateway.docker.internal`. Container DNS goes through the Mac's own resolvers (UDP and TCP), including per-domain ones from `scutil --dns` (`/etc/resolver/*`, VPN split DNS, Tailscale MagicDNS), so VPN and link-local DNS work.
- `docker run -i`/`attach`/`exec` with stdin half-close, healthcheck-gated `depends_on`, and idempotent `compose up`.
- `rekcod import`: images (`save | load`) and named volumes (data, labels; copied with `docker cp` and checked by file count and bytes), so compose reuses them. Volumes in use at the source are skipped unless you pass `--force`.
- Idle VM memory is about 160 MiB RSS.
- Database-style workloads run at native-Mac durability: guest `fsync` maps to macOS `fsync` (survives VM or app crashes, like any Mac app), not a full media flush on every commit. A power cut or kernel panic can lose the last writes, as for any Mac app; the guest runs `e2fsck -p` on the data disk at every boot. pgbench (8 clients, scale 20): about 15k TPS, versus 1k with Virtualization.framework's default. `REKCOD_DISK_SYNC=full` restores full flushes.
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
rekcod snap save seeded --push     # also uploads it to S3 (below)
rekcod snap restore seeded --pull  # on another Mac: downloads it first, then restores
```

The project is the compose project started from the current directory (`-p` to pick one).
Snapshots are copies on the VM's disk (a 360 MB Postgres data dir saves or restores in
under a second). A restore after `compose down -v` recreates the volumes so compose adopts them.
A save writes a new copy and only deletes the old one once every volume copied, so a save
that fails or is interrupted leaves the previous snapshot as it was. A restore first saves
the current volumes as snapshot `pre-restore`, so `rekcod snap restore pre-restore` undoes it.
`--push` and `--pull` use `$S3_ENDPOINT`/`$S3_BUCKET` with the `AWS_*` keys when they're set
(an app's bucket, say), else `rekcod storage up`'s RustFS and bucket `rekcod-snaps`. Each
volume goes up as `snaps/<project>/<name>/<volume>.tar.zst`.

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

### Dashboard

`dashboard/` is a web version of the menu bar with room for the fleet: containers grouped by
compose project with live CPU and memory, images, volumes and a disk breakdown, apps with
traffic, releases (and their deploy logs) and instances, jobs and schedules, nodes, routes,
backups, bans, sandboxes and snapshots, plus a "needs attention" list. Every detail is a page
of its own (`/containers/web`, `/apps/blog`, `/nodes/n-1`).

```sh
rekcod dashboard     # the bundled build, on http://localhost:4500 or the next free port
# or from a checkout (prints the sign-in link):
cd dashboard && npm ci && npm run dev
```

It runs commands as you, and every container can reach the Mac's 127.0.0.1, so each launch
makes a new token. `rekcod dashboard` opens `/auth?token=...`, which swaps it for a cookie;
without that cookie every page and API route answers 401. The token is in
`~/.rekcod/run/dashboard.json` (0600), and running `rekcod dashboard` again opens the running
one signed in.

Docker events push changes into the page (a container stopped in a terminal shows in about
half a second) and the fleet refreshes every 15 s. Every button runs one of the actions
`rekcod state` lists, the same ones the menu bar offers; logs stream on a page of their own,
shells open Terminal, and everything run is kept on an Activity page. Details:
[`docs/dashboard.md`](docs/dashboard.md).

### Sandbox

`rekcod x --sandbox <command>` runs in a second, throwaway VM whose only Mac
directory is the current one. The rest of `$HOME` (`~/.ssh`, cloud credentials, other
repos) doesn't exist there, and nothing on the Mac or the local network is reachable
(checked by the rekcod daemon on the Mac, not only inside the VM); the internet is.
Variables get in only with `--secret NAME`; `--offline` cuts the network.
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
rekcod sandbox apply $id --reviewed  # exactly what the last diff showed
rekcod sandbox rm $id
```

The internet is limited to an allowlist of package and image registries and GitHub
(`--allow-host` adds names, `--allow-host '*'` opens it, `--offline` cuts it).
Names off the list don't resolve, connections to addresses no allowed name resolved to
are refused, and HTTPS to an allowed address must name an allowed host in its SNI (shared
CDN addresses serve other sites too). The rekcod daemon on the Mac makes those checks: the
sandbox's network card is a socket it serves, so root inside the sandbox flushing its own
iptables changes nothing ([`docs/sandbox-network.md`](docs/sandbox-network.md)).
`rekcod sandbox log ID --net` shows what was blocked. A sandbox powers off
after 30 minutes without an exec (`--ttl`) and boots again on the next one.

For agents: `rekcod mcp` is an MCP server on stdio with tools for create, exec,
read/write file, fork, diff, apply, stop and remove (`claude mcp add rekcod -- rekcod mcp`).
Callers can only create sandboxes for the directory the server started in (`--root DIR`
adds others), and can't open the network, mount the live directory or pass secrets unless
the server was started with `--allow-host`, `--allow-mount` or `--secret`.
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
into the guest's `~/ws` and back with tar). Their network is plain NAT with no filtering:
the Mac and the LAN are reachable, so `--offline` and `--allow-host` are refused for them. The MCP server adds `sandbox_screenshot` and `sandbox_input` for
computer-use agents. Details: [`docs/platform.md`](docs/platform.md#track-4-as-built).

### A spare Mac

One command on your Mac makes a spare Mac a deploy target and a docker host that runs with its
lid shut:

```sh
ssh-copy-id me@spare-mac
rekcod host add spare me@spare-mac --fleet --domain apps.example.com
rekcod app deploy --remote spare          # http://<spare's tailnet name>:8080, Host: <app>.apps.example.com
docker ps                                 # runs on the spare too
```

The spare needs Remote Login, Tailscale on your tailnet (or `--addr IP`) and automatic login,
because the services are LaunchAgents and those only run in a logged-in session. rekcod doesn't
have to be installed there: `host add` copies this binary to `~/.rekcod/bin/rekcod` if it's
missing. Then it runs `rekcod spare setup` over SSH, which installs RustFS, the controller, node and
edge as LaunchAgents on the Tailscale address and runs `sudo pmset -a sleep 0 disablesleep 1
autorestart 1` so the lid can shut and a power cut doesn't leave it off (sudo asks for the
spare's password; `--no-power` skips it). It adds the remote `spare` here, then starts the
spare's VM and adds the docker host (`--no-docker` skips that). Re-running it is safe.
`rekcod spare uninstall` on the spare removes the agents. Details: [`docs/spare.md`](docs/spare.md).

The setup prints the `cloudflared` commands for public apps (`rekcod app public`).

### Docker on another Mac

`rekcod host` makes a spare Mac's rekcod VM your docker engine, over plain docker-over-SSH.
On the spare, run `rekcod start` and turn on Remote Login (System Settings › General ›
Sharing). Then on your Mac:

```sh
ssh-copy-id you@spare-mac              # docker's SSH helper can't answer a password prompt
rekcod host add spare you@spare-mac    # sets up the spare, creates docker context rekcod-spare, switches to it
docker ps                              # runs on the spare
rekcod host forward spare              # the spare's published ports on 127.0.0.1 here, until Ctrl-C
rekcod host use local                  # back to this Mac's rekcod
```

`host add` writes a marked block to the spare's `~/.zshenv`. For non-interactive SSH only, it
puts the docker CLI on `PATH` (non-login SSH has no `/opt/homebrew/bin`) and points
`DOCKER_HOST` at `~/.rekcod/run/docker.sock`, so it doesn't matter which context the spare
has active. The spare's login shell has to be zsh; otherwise `add` prints the two lines to
add by hand. `forward` follows containers as they start and stop, and reports a port that's
already taken here once instead of retrying it.

Bind mounts (`-v ./src:/app`) are paths on the spare, and `docker build` sends its context
over SSH. The menu bar shows a Docker host submenu for switching and forwarding, but the rest
of the menu is still this Mac's VM. Details: [`docs/remote-docker.md`](docs/remote-docker.md).

### Fleet and app hosting

The same sandboxes can run on a team's own Macs, and apps can be deployed onto them. One
controller (state in a SQLite file, `~/.rekcod/controller/fleet.db`) places sandboxes and app instances on nodes; `rekcod edge`
routes `<app>.<domain>` to them. Design: [`docs/platform.md`](docs/platform.md). Read the
licence section there first: Apple's macOS licence restricts running it as a service.

```sh
# the controller (any Mac); prints its CA fingerprint and where the admin and join tokens are
rekcod controller serve --listen 0.0.0.0:7700 --domain apps.example.com --tls-name controller
# each Mac, then approve it
rekcod node join https://controller:7700 "$(cat join-token)" --ca-fingerprint sha256:... \
  --listen 0.0.0.0:7701 --advertise https://mac2:7701
rekcod node approve n-abc123 --remote team
# you
rekcod remote add team https://controller:7700 --token ADMIN_TOKEN --ca-fingerprint sha256:...
rekcod controller token alice --max-sandboxes 5          # a user token (--expires 30d, `controller revoke alice`)
rekcod sandbox --remote create                           # clones this repo's origin at HEAD on a node
rekcod sandbox --remote exec ID -- npm test
rekcod controller token edge-1 --edge                    # what an edge needs and nothing more
rekcod edge serve --remote edge --listen 100.x.y.z:8080 --public-listen 127.0.0.1:8081   # all apps / public apps only
rekcod app deploy                                        # this repo's pushed HEAD + rekcod.toml
rekcod app rollback dash
rekcod app env set dash NODE_ENV=production              # redeploys the live release
rekcod app public dash                                   # also serve it on --public-listen
```

The controller makes its own CA on first start (`controller/ca.pem`) and serves TLS with a
certificate for its listen address, host name, `localhost` and each `--tls-name`. Remotes
and nodes fetch the CA once and keep it only if it matches `--ca-fingerprint`; after that
every control-plane connection (CLI, edges and nodes to the controller, the controller to
nodes) is verified TLS. Nodes get their certificate from the controller when they join.
`rekcod controller ca` prints the fingerprint again. `controller serve --http` serves plain
HTTP, and only on loopback or tailnet addresses (100.64.0.0/10, `*.ts.net`) unless
`REKCOD_INSECURE=1`. App traffic between the edge and nodes is plain HTTP, so keep nodes on
a tailnet or LAN you trust.

New nodes wait as `pending` until `rekcod node approve ID` (`controller serve --auto-approve`
skips that, as `spare setup` does). `node rm ID` forgets one and its instances move.
`controller rotate-join` replaces the join token. Tokens are stored hashed. An `--edge`
token reads routes and bans, reports traffic and wakes sleeping apps; a token made with
`--ns` has no access to `default`.

Apps are private until you make them public (`app public NAME`, `app deploy --public`, or
`public = true` under `[app]` in `rekcod.toml`). `--listen` routes every app; bind it to
your tailnet or loopback. `--public-listen` routes only public apps; point a tunnel at it,
for example `cloudflared` with `ingress: [{hostname: "*.example.com", service:
http://127.0.0.1:8081}]` and the controller started with `--domain example.com`. A private
app answers `no app at ...` there, the same as one that doesn't exist. PR previews are
private.

The edge routes every request on its own Host header, also on a keep-alive connection a
tunnel shares between hostnames. Each client IP gets 50 requests/s across all apps (burst
100; `--rate 20/s --burst 40` changes it), and over that it gets a `429` with `Retry-After`.
An IP that racks up 100 of those in a minute is banned on every edge for 10 minutes. An app can
set its own limit and body cap:

```toml
[http]
rate = "20/s"        # per client IP, on top of the edge's; N/s, N/m or N/h
burst = 40
max_body = "10MB"    # 413 over this; 100 MiB by default
```

```sh
rekcod edge ban 203.0.113.0/24 --reason scraper   # every edge, within 2 s
rekcod edge ban 198.51.100.7 --for 1h
rekcod edge bans
rekcod edge unban 203.0.113.0/24
```

A banned IP's connection is closed before anything is read. Behind cloudflared every
connection comes from the tunnel, so the edge takes the client IP from `CF-Connecting-IP` or
`X-Forwarded-For` when the peer is a trusted proxy. That's loopback by default, which covers
cloudflared on the same Mac. A banned visitor behind it gets a `403`, and the tunnel itself is
never banned. If the tunnel runs elsewhere, use `--trusted-proxy 10.0.0.5` (repeatable; it
replaces the loopback default). Headers from any other peer are ignored, so nobody can pick
their own IP. Use `--proxy-protocol` when a TCP load balancer in front of `--public-listen`
sends PROXY v2. Limits are counted per edge process. Keep-alive connections are
closed after 90 s without a request, and `--access-log` prints a line per request. Design:
[`docs/edge-http.md`](docs/edge-http.md).

`--install` on `controller serve`, `node join`/`node serve`, `edge serve` and `storage serve`
runs each as a LaunchAgent; `rekcod controller|node|edge|storage uninstall` removes it.
[A spare Mac](#a-spare-mac) sets up all four with one command. With storage the controller
backs `fleet.db` up every hour (below); `--backup-dir DIR` also keeps those copies on another
disk or a synced folder. Without either, copy `fleet.db` (and its `-wal` file) while it's
stopped. Either way keep a copy of `controller/secret-key` somewhere else: app secrets in
`fleet.db` are sealed with it, and they can't be read without it. A controller that finds
sealed secrets but no key refuses to start rather than make a new one.

The controller keeps the fleet converging on what it should run. Every 10 s it health-checks
instances: one that fails three checks leaves the routes, one on a node that comes back
returns once it answers, and an app short of healthy instances for a minute is redeployed
at its live commit (from stored images, so no rebuild). Nodes boot their instance VMs when
they start, and report their sandboxes in every heartbeat. A restart marks deploys that
were in flight failed. `--alert-webhook URL` posts `{"text": ...}` when a node is lost or a
deploy or backup fails (Slack and Discord take that), and `GET /metrics` (admin or edge
token) is Prometheus text. Stored release images are kept for the newest 10 commits per app.

### Namespaces

A namespace is a team's apps, sandboxes, jobs, secrets and quota on a shared fleet. An app
`web` in namespace `billing` is served at `web--billing.<domain>`; apps in `default` keep
`<app>.<domain>`.

```sh
rekcod ns create billing --max-cpus 16 --max-memory 32 --remote admin   # admin token; any limit can be `unlimited`
rekcod ns member add billing alice --role member                         # viewer, member or owner
rekcod controller token dave --ns billing --role viewer                  # new token, already a member
rekcod controller limit dave --max-sandboxes unlimited                   # change a token's own cap
rekcod ns use billing                     # this remote's app/sandbox/job commands go to billing
rekcod app deploy --ns billing            # or per command (REKCOD_NS works too)
rekcod ns ls                              # your namespaces, role, quota and usage
rekcod node pool n-abc123 --ns billing    # only billing runs on that node; no --ns undoes it
rekcod ns rm billing --force              # destroys its apps, sandboxes and jobs
```

Viewers can list, read status and logs. Members deploy, set env and secrets, and run
sandboxes and jobs. Owners also manage members. Everyone with a token is in `default`, and
there each person only sees their own things, as before namespaces. In any other namespace
members share everything. A namespace you aren't in answers `no namespace`, the same as one
that doesn't exist. Quotas count apps with a live release (sleeping ones too), running
sandboxes and jobs. Nodes only accept connections to app instances from the controller and
from edges that have fetched routes, so nobody on the tailnet can skip the edge. Design:
[`docs/namespaces.md`](docs/namespaces.md).

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
idle_stop = "15m"      # stop the VMs after 15 min without a request; off by default
recycle = "24h"        # fresh VMs every day; off by default
[env]
GREETING = "hi"
```

A VM doesn't give memory back to macOS while it runs, so these two are how a fleet gets it
back. `idle_stop` routes the app away and stops its VMs; the disks stay. The next request
through the edge is held while the controller boots them and waits for the health check,
then it's forwarded. In `scripts/e2e-fleet.sh` that took 1 to 2 s for a small Python app. Only
HTTP through the edge wakes an app, so background loops and open WebSockets stop with it.
PR previews sleep after 15 minutes unless the base app sets its own `idle_stop`. `recycle`
is a normal zero-downtime redeploy of the live commit, so it rebuilds. `rekcod app wake` and
`rekcod app recycle` do either by hand. Details: [`docs/fleet-compute.md`](docs/fleet-compute.md).

Jobs run a command to completion in a fresh sandbox, then delete it:

```sh
rekcod job run --memory 16 --cpus 8 --timeout 2h -- python etl.py      # follows output, exits with its code
rekcod job run --name nightly --every 24h -- sh -c "pip install -r requirements.txt && python etl.py"
rekcod job ls
rekcod job logs j-abcd1234
rekcod job cancel j-abcd1234
rekcod job unschedule nightly
```

The workspace is the repo's pushed HEAD (or `--git/--ref`). The image comes from the version
files like `rekcod x` (`--image` overrides it, alpine otherwise), and there's no install
step, so put it in the command. `--timeout` exits 124. The controller keeps the last 64 KiB
of output. A schedule never runs two at once, and running jobs count toward the token's
`--max-sandboxes`.

Point a GitHub webhook (pull_request events, the secret in `controller/webhook-secret`) at
`/webhooks/github` and each PR gets `<app>-pr-<n>`, deleted when it closes or after 7 days.
`rekcod node` replaces `docker node` here; use `rekcod docker node` for swarm.

### S3 storage

RustFS is the fleet's bucket: one S3-compatible binary next to the controller. It gives apps
somewhere to keep files, keeps built images so rollbacks don't rebuild, and holds the
controller's backups.

```sh
rekcod storage serve --listen 100.x.y.z:7710 --install          # downloads RustFS 1.0.1, keys in ~/.rekcod/storage/credentials.json
rekcod controller serve ... --storage http://100.x.y.z:7710     # remembered in fleet.db after the first start
```

An app asks for a bucket in `rekcod.toml`:

```toml
[storage]
bucket = true
```

Its instances get `S3_ENDPOINT`, `S3_BUCKET` (`app-<name>`), `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY` and `AWS_REGION=us-east-1`. The key only works on that bucket. The
endpoint takes path-style requests, so set `forcePathStyle: true` in the AWS SDK. PR
previews get their own empty bucket, deleted with the preview.

```sh
rekcod app storage dash              # endpoint, bucket, key, object count and bytes
rekcod app storage dash ls
rekcod app storage dash url avatars/1.png --expires 10m   # presigned GET
rekcod app storage dash rotate       # new secret; redeploys the live release with it
rekcod app destroy dash              # asks before deleting a bucket that isn't empty (--force doesn't)
```

After each build the node uploads the image (`docker save | zstd`) to
`rekcod-releases/<app>/<commit>.tar.zst`. Rollbacks, `recycle`, scale-ups, wakes that need a
redeploy, and the second and later instances of a deploy load it instead of cloning and
building. BuildKit's cache goes to `rekcod-cache/<app>.tar.zst`, so a build on another node
starts warm. The controller uploads `fleet.db` to `rekcod-backups/` every hour and keeps 48:

```sh
rekcod controller backup             # one now
rekcod controller backups
rekcod controller restore            # stop the controller first; the latest unless you name one
```

For local development, `rekcod storage up` runs RustFS in the VM on a free port from 9000
(it never touches whatever has 9000) and prints the env block; `storage env` prints it again
and `storage down` stops it, keeping the data in volume `rekcod-storage`. Design and
limitations: [`docs/rustfs.md`](docs/rustfs.md).

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
| `rekcod start [--cpus N] [--memory GiB] [--disk GiB] [--no-switch]` | Boot the VM, create and select the `rekcod` docker context. `--cpus` defaults to the Mac's performance cores (at least 4); `--disk` (default 256, sparse) grows the data disk, never shrinks it. The values are saved in `~/.rekcod/config.json` and reused when the menu bar or a docker command boots the VM |
| `rekcod start --install\|--uninstall` | Start the VM at login and boot it again if it stops without `rekcod stop` (LaunchAgent `dev.rekcod.vm`) |
| `rekcod start --network auto\|user\|nat` | How the VM reaches the network. `nat` is Virtualization.framework's NAT; `user` makes rekcod open the connections itself, which full-tunnel VPNs (Mullvad and others) don't drop; `auto`, the default, picks `user` while the Mac's default route goes through a VPN tunnel. Saved; `rekcod status --json` shows which is in use |
| `rekcod start --repair` | When the VM won't start because its data disk has errors: `e2fsck -fy` on it and a fresh root disk, then boot. The data disk is never formatted unless it's blank |
| `rekcod stop [--no-switch] [--force]` | Graceful power-off, restore the previous context. `--force` powers off a hung VM right away |
| `rekcod status [--json]` | Versions, VM size, forwarded ports (and any that couldn't be bound), data-disk use in the VM and on the Mac, file-cache trips to macOS, warming in progress, the sandbox VM. `--json` is the scriptable form |
| `rekcod snap save\|restore\|ls\|rm [NAME] [-p PROJECT] [--push\|--pull]` | Volume snapshots of a compose project, optionally through S3 (above) |
| `rekcod x [--sandbox] [--image IMG] [-p PORT] <command>` | Run a command in a container, no Dockerfile (above) |
| `rekcod sandbox create\|exec\|shell\|cp\|fork\|diff\|apply\|ports\|log\|ls\|stop\|rm\|serve\|golden` | Agent sandboxes (above) |
| `rekcod mcp [--root DIR] [--allow-host H] [--secret NAME] [--allow-mount]` | MCP server on stdio for the sandbox commands; the flags fix what callers may ask for |
| `rekcod macos fetch\|install\|setup\|authorize\|ls\|rm` | macOS base images for `sandbox create --os macos` |
| `rekcod sandbox screenshot\|input` | Screen and mouse/keyboard of a macOS sandbox |
| `rekcod remote add NAME URL --token T [--ca-fingerprint FP]\|ls\|rm\|use` | Fleet controllers this CLI talks to (`--remote` on `sandbox` and `app`); `use` sets the default |
| `rekcod ns create\|ls\|quota\|member\|use\|rm` | Namespaces on the fleet (`--ns` on `sandbox`, `app` and `job`) |
| `rekcod node join\|serve\|ls\|approve\|rm\|drain\|undrain\|pool\|uninstall` | Run this Mac as a fleet node; list, approve, remove, drain, undrain or dedicate nodes |
| `rekcod controller serve\|ca\|token\|revoke\|rotate-join\|limit\|backup\|backups\|restore\|uninstall` | The fleet controller; its CA fingerprint, user and edge tokens, their caps, the join token, backups of `fleet.db` |
| `rekcod storage serve\|uninstall\|up\|down\|env` | RustFS: the fleet's S3 bucket, or one in the local VM |
| `rekcod edge serve\|ban\|unban\|bans\|uninstall` | HTTP ingress for apps; ban IPs and ranges on every edge |
| `rekcod app deploy\|ls\|status\|release\|public\|private\|rollback\|scale\|env\|secret\|logs\|open\|wake\|recycle\|storage\|destroy` | Apps on the fleet; `release NAME N` prints one release with its deploy log, `logs --instance ID` one instance's logs (also after a redeploy removed it: the last lines of the 20 newest gone instances are kept) |
| `rekcod job run\|ls\|logs\|rerun\|cancel\|unschedule` | Commands that run to completion on the fleet, once or `--every`; `ls --name N` is one job's last 50 runs, `logs -f` follows a running one, `rerun ID` runs a finished one again |
| `rekcod host add\|ls\|use\|rm\|forward` | Another Mac's rekcod as the docker engine, over SSH; `add --fleet` also provisions it as a fleet |
| `rekcod spare setup\|uninstall` | This Mac as a headless one-machine fleet (RustFS, controller, node, edge, no sleep) |
| `rekcod socket install\|uninstall` | Route `/var/run/docker.sock` to rekcod while it runs (sudo once) |
| `rekcod import --from <context> [NAMES...] [--images-only\|--volumes-only] [--force]` | Copy images and named volumes from another engine |
| `rekcod shell` | Root shell in the VM |
| `rekcod menubar [--install\|--uninstall]` | Menu bar item with the same actions as the CLI: VM start/stop/compact/shell, local S3 storage (start, env, stop), containers (start, stop, restart, logs, remove), ports (click to open), compose projects (restart, stop, down), snapshots (save, restore, delete), images and volumes (remove, prune), agent sandboxes (shell, diff, apply, stop, delete), apps on the default remote (open, logs, roll back, wake, restart instances, destroy), jobs (output, run again, cancel), nodes (drain, undrain), remotes (switch, back up the controller), edge bans. `--install` starts it at login (LaunchAgent `dev.rekcod.menubar`) |
| `rekcod state` | Everything the menu bar and dashboard show, with each item's actions (`argv`, `confirm`), as one JSON document; `--local` skips the fleet controller |
| `rekcod dashboard [--install\|--uninstall]` | Start the web dashboard (or find the running one) and open it; `--install` runs it at login (LaunchAgent `dev.rekcod.dashboard`) |
| `rekcod upgrade [--check]` | Install the latest release over this binary |
| `rekcod doctor [--bundle]` | Check what usually goes wrong, with a fix for each; `--bundle` writes a diagnostics tarball |
| `rekcod uninstall [--keep-data] [--yes]` | Remove everything rekcod set up |
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
later builds reuse it until `guest/kernel`, `guest/fuse`, `guest/build.sh` or the pins in
`guest/versions.env` change.
See `docs/guest-build.md`.

```sh
scripts/build.sh                                   # host binary (signed), agent, out/guest
KERNEL=alpine scripts/build.sh                     # Alpine's stock linux-virt instead
scripts/build.sh --host-only                       # skip the guest image
scripts/package.sh                                 # release tarballs + SHA256SUMS in out/release
```

Releases: push a `vX.Y.Z` tag matching `Cargo.toml`. `.github/workflows/release.yml`
runs all of CI first, then packages the host with the dashboard on macOS, attaches SBOMs
and a provenance attestation, and publishes the release ([`docs/releasing.md`](docs/releasing.md)).
CI (`ci.yml`) runs rustfmt, clippy, the host tests on macOS, the agent's Linux-only tests on
arm64 Linux, `cargo deny`, the dashboard's `npm audit`, unit, type, lint and Playwright
checks, and a guest build. The e2e suite needs a Mac that can run VMs, so it runs locally;
`e2e.yml` runs it on a self-hosted Mac once one is registered.

`target/release/rekcod` finds `out/guest` on its own. Elsewhere, set `REKCOD_GUEST_DIR` or copy
`out/guest` to `~/.rekcod/guest`.

## Test

`git config core.hooksPath .githooks` once per clone runs CI's host checks (fmt, clippy, tests) before every commit.

```sh
cargo test --workspace                       # unit tests
cargo fmt --all --check                      # formatting (rustfmt.toml; CI enforces it)
cargo deny check                             # advisories, licenses, sources (deny.toml; CI runs it)
(cd dashboard && npm test && npm run typecheck && npm run lint)  # dashboard
scripts/e2e.sh                               # real CLI + compose against rekcod; ends with `rekcod stop`,
                                             # refused while containers it didn't start run (E2E_ALLOW_STOP=1)
E2E_IMPORT_FROM=orbstack scripts/e2e.sh      # also exercises import
python3 bench/headroom.py --context rekcod   # file-sharing benchmark (compare --context orbstack)
python3 bench/inotify_rtt.py --context rekcod  # Mac edit -> container inotify latency
bench/cold_start.sh                          # first access right after boot, warmed vs not
bench/postgres.sh rekcod desktop-linux       # pgbench on a named volume, per docker context
python3 bench/tcp_rtt.py --context rekcod    # 64-byte round trip through a published port
bench/sandbox.sh                             # agent sandbox create/fork/rm timings, 16 at once
scripts/e2e-sandbox.sh                       # agent sandbox checks only; leaves the main VM alone
scripts/e2e-macos.sh                         # macOS sandbox checks; needs a ready base image
scripts/e2e-fleet.sh                         # controller, 2 nodes, edge, apps; own homes and database
scripts/e2e-storage.sh                       # RustFS: app buckets, stored images, backups; own homes
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

- `crates/rekcod`: CLI plus the daemon that owns the VM (`objc2-virtualization`), the socket proxy (`proxy.rs`, rewrites `containers/create` for `dev.rekcod.local`), the port forwarder (published ports and `rekcod x` listeners), file events, the DNS bridge and clock sync. `snap.rs` and `xrun.rs` implement `rekcod snap` and `rekcod x`; `actions.rs` is the action list that `menubar.rs` renders and `state.rs` prints for the dashboard (`dashboard/`, Next.js).
- `crates/rekcod-agent`: static musl guest agent.
- `crates/rekcod-proto`: vsock ports and wire formats.
- `guest/`: a 6.12 LTS kernel built from Alpine's `linux-virt` config plus `guest/kernel/rekcod.config` (virtio, ext4, fuse and docker's netfilter built in, no initramfs; [docs/custom-kernel.md](docs/custom-kernel.md)), an Alpine ext4 root with upstream static docker binaries, and busybox init. `--build-arg KERNEL=alpine` builds the stock-kernel guest instead.
- `guest/fuse/`: the fuse/virtiofs patch (long TTLs, lazy open, host-driven invalidation), applied in-tree to the guest kernel.
- State lives in `~/.rekcod`: `vm/root.img` (replaced on upgrade), `vm/data.img` (256 GiB sparse by default, all docker data), `vm/warm-paths` (bind sources warmed at boot) and `logs/`. The sandbox VM has its own copy under `~/.rekcod/sandbox`.

Design and roadmap: [`docs/mvp.md`](docs/mvp.md), [`docs/plan.md`](docs/plan.md), [`docs/phase2.md`](docs/phase2.md), [`docs/phase2b-4.md`](docs/phase2b-4.md), [`docs/research.md`](docs/research.md), [`docs/phase5.md`](docs/phase5.md), [`docs/sandbox.md`](docs/sandbox.md), [`docs/frontends.md`](docs/frontends.md), [`docs/paas.md`](docs/paas.md), [`docs/agent-sandboxes.md`](docs/agent-sandboxes.md), [`docs/platform.md`](docs/platform.md). Next steps: [`docs/handoff.md`](docs/handoff.md).

## Limitations

- First access can be slower than OrbStack for a tree rekcod hasn't seen since boot, when a container touches it before background warming finishes (about 1 s per 20k files). Recently used trees are warmed at boot (a 19.5k-file walk 3 s after `rekcod start`: 34 ms warmed vs 286 ms not). Repeat work is 3–9× faster.
- Mac→container change latency is about 14 ms p50 (`bench/inotify_rtt.py`; OrbStack measured 12 ms).
- The VM's directories (`~/.rekcod/vm`, `sb`, `golden`, macOS images) are excluded from Time Machine; back up what's in your volumes with `rekcod snap save --push` or your own dumps.
- Memory isn't returned to macOS. `REKCOD_BALLOON=1` drives the balloon, but on macOS 27 the VM's footprint doesn't drop, even under memory pressure (docs/phase2.md, docs/phase5.md).
- `--sandbox` and `rekcod sandbox` haven't had an external security review (docs/sandbox.md); [`docs/security-review.md`](docs/security-review.md) is the scope for one.
- A sandbox fork copies the disk, not memory: processes restart in the child (Virtualization.framework can't restore a VM with a virtio-fs share).
- No brew formula. Releases are ad-hoc signed, not notarized.

Debug knobs:

- `REKCOD_DEBUG=1` logs forwarded file events.
- `REKCOD_KERNEL_CMDLINE` overrides the guest command line.
- `REKCOD_NO_BALLOON=1` drops the balloon device.
- In the guest, `cat /sys/module/virtiofs/parameters/rekcod_stats` shows host requests by FUSE opcode.

## License

Apache-2.0, see [`LICENSE`](LICENSE).
