# rekcod platform: full spec

**Status:** spec, 2026-10-12. Research and measurements are in [`paas.md`](paas.md).
Phase 1 is detailed in [`agent-sandboxes.md`](agent-sandboxes.md). This doc covers the
whole platform: all four tracks, what each one needs, and the gate before each starts.

## Tracks

| # | Track | Runs on | Who uses it | Gate before starting |
|---|---|---|---|---|
| 1 | Local agent sandboxes | the user's Mac | one developer and their agents | **built** 2026-10-12; [`agent-sandboxes.md`](agent-sandboxes.md) |
| 2 | Self-hosted fleet | a team's own Mac minis | a team's engineers and agents | legal read of SLA §2I |
| 3 | App hosting (PaaS) | the fleet from track 2 | internal apps and preview environments | track 2 in use |
| 4 | macOS guests: CI and computer-use | dedicated Macs | iOS/macOS builds, agents driving macOS apps | track 2 in use, plus a demand check |

Each track reuses the one before it. Sandboxes are the unit: a deployed app is a sandbox
that's never torn down and has an ingress route. A fleet runs sandboxes on more than one
Mac. A macOS guest is a sandbox with a different guest OS and stricter placement.

Selling any of this to outside tenants is not a track (see "Hosted multi-tenant" at the
end).

## Constraints every track inherits

- **Licence (macOS 27 SLA).**
  - §2B(iii): two macOS guests per Mac, for development, testing or personal
    non-commercial use.
  - §2I: no "service bureau, time-sharing" use of macOS, "whether for your own organization
    or third parties".
  - §3: leasing is for CI only, at least 24 hours, one lessee per Mac, with notice to
    Apple.
  - Linux guests aren't capped by the framework, but §2I may still reach them.
- **Memory isn't returned to macOS** while a VM runs (`phase5.md`). Size VMs for their peak
  and recycle long-lived ones. Never count on overcommit.
- **No live snapshots.** Virtualization.framework save/restore doesn't work with virtio-fs
  (`plan.md` §10). Fork and restore copy the disk and cold-boot in about 1 s.
- **Hardware.** Mac minis have no ECC, no BMC and one power supply. Treat each one as
  disposable: state that matters lives off the Mac or is replicated.
- **Measured density (M5 Pro, 48 GB).** 32 VMs ran at once at about 280 MB idle each, and
  24 started together were all ready within 4 s.

## Track 1: local agent sandboxes

Specified in [`agent-sandboxes.md`](agent-sandboxes.md). In summary:

- `rekcod sandbox create|exec|shell|cp|fork|diff|apply|ports|log|ls|stop|rm`
- golden disks
- copy-on-write workspaces
- an egress allowlist
- an exec log
- an HTTP API on a unix socket and an MCP server
- a menu bar section

Six milestones.

Tracks 2–4 depend on three things from it:
- `sandbox.rs` as a library with no CLI assumptions;
- `meta.json` as the sandbox record;
- the API shape, which the fleet controller exposes unchanged.

## Track 2: self-hosted fleet

### What it is

A team racks Mac minis (or uses existing ones). One controller schedules sandboxes across
them. Developers and agents call the same API as track 1, pointed at the controller
instead of the local socket.

### Pieces

| Piece | What it does |
|---|---|
| `rekcod node` | runs on each Mac as a LaunchDaemon. Wraps `sandbox.rs`, reports capacity, pulls golden disks, accepts work from the controller |
| `rekcod controller` | the API from track 1 plus placement, auth, quotas and the fleet's state. Runs on Linux or one of the Macs |
| State store | Postgres. Holds nodes, sandboxes, users, tokens, quotas and an audit log |
| Golden disk registry | an S3-compatible bucket (RustFS in dev and CI) of compressed golden `data.img` files, versioned by content hash |
| Workspace transport | `git clone` of a ref, or a tarball upload. There's no `clonefile` across machines |

### Node

- **Registration.** On boot, the node registers with the controller over mTLS: hardware,
  macOS version, free RAM, CPUs and the golden disks it has. Certificates come from the
  controller's own CA, issued by a one-time join token: `rekcod node join URL TOKEN`.
- **Heartbeat.** Every 5 s, carrying sandbox states, the memory footprint of each VM and
  free disk. Three missed heartbeats mark the node `lost`, and its sandboxes go to `lost`
  too. They aren't rescheduled automatically, because their disk was on that Mac.
- **Capacity.** It refuses work above 75% of RAM, the same rule as track 1, so the
  controller's view and the node's view can't disagree in a way that swaps the Mac.
- **Drain.** `rekcod node drain` stops placement on the node and waits for its sandboxes to
  end, or for their TTLs to expire.
- **Upgrade.** The node runs one rekcod version at a time. Upgrade by draining, replacing
  the binary, then re-joining. Golden disks are rebuilt for the new guest version.

### Controller

- **API.** It serves the track 1 endpoints, adding `node` to the sandbox object and
  `?node=` to filter.
  - `exec` and `files` are proxied to the node over the mTLS connection.
  - Streaming exec output passes through as NDJSON.
  - It also keeps `/nodes`, `/nodes/{id}/drain`, `/users` and `/tokens` for admins.
- **Placement.** It sends a sandbox to the node with:
  1. the requested golden disk already present;
  2. enough free RAM for the requested size;
  3. the most free RAM.

  Fork goes to the parent's node, because it's a local `clonefile`. If that node is full,
  fork fails with `node full`; the controller doesn't copy the disk elsewhere.
- **Auth.**
  - Each user gets a token, or comes in through OIDC from the team's identity provider.
  - Agents use scoped tokens: create and exec on sandboxes they made, never `apply`, never
    another user's sandbox.
  - Every API call is written to the audit log with token, sandbox, action and result.
- **Quotas.** Per user and per team: running sandboxes, total RAM and maximum TTL. A
  request over quota fails with the numbers.
- **Workspaces.** `create` takes `{"git": {"url", "ref"}}` or an uploaded tarball. Git
  credentials are a controller-side secret, injected for the clone only and not left in
  the sandbox. `diff` returns a patch. `apply` isn't offered remotely; the developer runs
  `git apply` locally.
- **Egress.** The same allowlist mechanism as track 1, with presets defined centrally. A
  team can add its internal package mirror, which also cuts pull traffic.

### Networking

- Each node has a private address on the team's network.
- Sandboxes keep the track 1 rules: no LAN, no Mac, allowlisted internet.
- Ports a sandbox listens on aren't reachable from other machines unless track 3's ingress
  routes them.

### CLI

```sh
rekcod node join URL TOKEN | drain | status
rekcod controller serve --db URL --bucket URL
rekcod remote add NAME URL --token T        # point the sandbox CLI at a controller
rekcod sandbox create --remote NAME ...     # every sandbox command takes --remote
```

Admin-only actions (`node drain`, token management) aren't in the menu bar. The parity rule
in `CLAUDE.md` covers user-facing actions. The menu bar lists remote sandboxes alongside
local ones, with the same `sandbox_actions`.

### Verification

- **CI test fleet.** The controller and two nodes run as separate `REKCOD_HOME`s on one Mac.
- **Placement.** Fill one node, and the next create goes to the other.
- **Lost node.** Kill a node, it goes `lost` within 15 s, and its sandboxes go `lost`.
- **Auth.** A scoped token can't exec in another user's sandbox.
- **Quota.** Over quota fails with the numbers.
- **Upgrade.** Drain and upgrade one node while the other keeps serving.

## Track 3: app hosting (PaaS)

### What it is

`git push`, or `rekcod deploy`, builds an app and runs it on the fleet behind HTTPS at
`<app>.<team domain>`. It's meant for internal tools, preview environments per branch and
staging. Public production traffic belongs on Linux hosts (see the end).

### Model

| Term | Meaning |
|---|---|
| App | a name, a git repo, a build config, env vars, secrets, a domain |
| Release | an immutable build: image digest plus config, numbered per app |
| Instance | a sandbox running one release, with `ttl: none` and an ingress route |
| Preview | an app created per branch or PR, deleted when the branch is merged or closed |

An instance is a track 1 sandbox with the workspace mode `none`. It has no project files,
only the release's image, and its `rekcod-ws` container is the app's container, or a
compose project if the repo has `compose.yaml`.

### Build

- The build runs in a short-lived sandbox on the fleet, using its `dockerd` and BuildKit.
  Inputs are the repo at a commit and an optional `rekcod.toml`.
- If there's a `Dockerfile`, it's built as is. If there isn't, the build uses the same
  version-file detection as `rekcod x` and generates one from the detected toolchain image
  plus the start command from `rekcod.toml`.
- The image is pushed to the fleet's registry: `rekcod registry`, a small OCI distribution
  server (`axum` plus `object_store`) with its blobs in the S3 bucket. Images are `linux/arm64`. `linux/amd64` works through Rosetta in the guest
  but is slower, and the build warns about it.
- Build cache is a BuildKit cache exported to the bucket per app, so a build on another
  node still hits cache.

```toml
# rekcod.toml, all optional
[app]
name = "dash"
port = 3000                  # default: the image's first EXPOSE
start = "npm start"          # only without a Dockerfile
health = "/healthz"          # HTTP 200 within 30 s, or the release fails

[resources]
cpus = 1
memory = "1GiB"
instances = 2                # spread across nodes

[env]
NODE_ENV = "production"
```

### Deploy

1. Build the release, or reuse it if that commit already has one.
2. Start N new instances, placed on different nodes where possible.
3. Wait for the health check.
4. Switch the ingress route to the new instances.
5. Stop the old ones after a 30 s drain.

If health fails, the old release keeps serving and the deploy reports the instance's logs.
`rekcod rollback APP [RELEASE]` repeats steps 2–5 with an existing release, with no build.

### Ingress

- `rekcod edge`, one per fleet, on a Linux box or one Mac, holds the public or VPN-facing
  address.
- TLS: `rekcod edge` issues certificates through ACME (`instant-acme`). Wildcard `*.<team domain>` uses the DNS-01
  challenge, so previews don't each need a new certificate.
- Routes: `rekcod edge` subscribes to the controller's route table over its mTLS connection, mapping each host to
  instance addresses as `node-ip:port`.
- On each node, `rekcod node` forwards that port to the instance. This is the existing port
  forwarder, bound to the node's private address instead of `127.0.0.1`, and only for
  instance sandboxes.
- Optional access control: `rekcod edge` checks a team session cookie against the controller, so internal apps and
  previews need a team login.

### Config and secrets

- `rekcod env set APP K=V` and `rekcod secret set APP NAME` store values in Postgres.
  Secrets are encrypted with a controller key.
- Changing either creates a new release from the same image and deploys it.

### Logs and metrics

- `rekcod logs APP [-f]` streams `docker logs` from every instance, prefixed with the
  instance ID.
- The node keeps 7 days of logs per instance on its disk; nothing is shipped centrally.
- `rekcod status APP` shows the release, instances, node, health, memory footprint and
  restarts.
- Nodes expose Prometheus metrics: VMs, footprint per VM, CPU, disk and sandbox counts.

### State

- Instances are stateless. A volume inside an instance dies with it, and the deploy
  docs say so.
- Databases: use an external Postgres (for me, the sandbox Postgres on Fly), or a
  compose service inside a single-instance app. In the second case it's pinned to one node
  and backed up with `rekcod snap` into the bucket on a schedule. That's a dirty
  implementation of durability, and it's for staging only.
- Volume replication between Macs isn't planned.

### Previews

- A webhook from GitHub creates `<app>-pr-<n>` on each PR push, comments the URL on the PR,
  and deletes the preview when the PR closes.
- Previews have a TTL (7 days default) and a team quota.
- An optional snapshot can seed preview databases from a golden `rekcod snap`, which is
  the per-branch database idea from `plan.md` §3a, on the fleet.

### CLI

```sh
rekcod deploy [--app NAME] [--remote NAME]     # from the repo in the current directory
rekcod apps | releases APP | rollback APP [N] | scale APP N
rekcod env set|unset|ls APP ...
rekcod secret set|rm|ls APP ...
rekcod logs APP [-f]
rekcod status APP
rekcod destroy APP
```

The menu bar gets an "Apps" section through a new `app_actions` list in `actions.rs`, with
open URL, logs, rollback (asks first), scale and destroy (asks first). Add each action's
`Kind`, then update the parity tests, `frontends.md` and the README in the same change.

### Verification

- A Node app with no Dockerfile deploys and answers on HTTPS.
- A second deploy is zero-downtime: a `curl` loop over it sees no non-200.
- A broken health check leaves the old release serving.
- Rollback works.
- A preview is created and deleted by PR events.
- A secret isn't visible in `rekcod status` or the logs.
- Killing a node running one of two instances keeps the app up.

## Track 4: macOS guests

### What it is

macOS VMs on fleet Macs for two uses that nobody serves well:
- iOS and macOS CI: Xcode builds, simulator tests, notarization.
- Computer-use agents driving real macOS apps, such as Safari testing or automating a
  desktop app.

### Licence shape

- At most two macOS guests per Mac. Virtualization.framework enforces it.
- Only for development and testing on the team's own Macs, which are the only machines
  track 2 covers.
- For outside customers, §3 applies: CI only, a whole Mac per customer, at least 24
  hours. Computer-use for outside customers doesn't fit §3 at all.

### Pieces

- **Guest images.** macOS restore images (IPSW) installed into a disk image once per macOS
  version and Xcode version. The macOS 27 framework adds guest provisioning
  (`VZMacGuestProvisioningOptions`) for unattended setup, and layered ASIF images with
  copy-on-write overlays. Use those instead of `clonefile` where they're available.
- **Guest agent.** A small launchd agent in the guest, the macOS counterpart of
  `rekcod-agent`. It handles exec, file copy and screenshots over vsock.
- **Sandbox type.** `rekcod sandbox create --os macos --image xcode-17`. The same API,
  where `exec` runs in the guest and `screenshot` and `input` (click, type, key) are added
  for computer-use agents through MCP tools.
- **Placement.** The controller tracks macOS slots per node (2) separately from RAM. A
  macOS sandbox needs a free slot and about 8 GiB.
- **Isolation.** The guest sees no Mac directories, and egress uses the allowlist,
  enforced by the vmnet topology from macOS 27 since there's no guest firewall rekcod
  controls.

### What rekcod brings and what it doesn't

The orchestration, API, MCP, fleet, placement, egress presets and audit log carry over. The
Linux guest work doesn't: the patched virtio-fs, `dockerd` and the bind-mount caches. Tart
already does macOS VM images well, but it's Swift; rekcod drives VZ from Rust already and is community-maintained under FSL now. Reusing its
image format is worth checking before writing our own.

### Gate

Start only when tracks 1–2 are in use and someone actually needs iOS CI or a macOS
computer-use agent. I'm not going to build it on speculation.

## Hosted multi-tenant: not planned

Selling sandboxes or app hosting to outside tenants on Macs is out. The licence (§2I and
§3) likely forbids it for anything but dedicated CI. E2B and Daytona charge $0.0504 per
vCPU-hour on Linux. And a Hetzner CAX41 (16 ARM vCPUs, 32 GB, €41/mo) undercuts a Mac mini
per core.

If we ever host for others, tracks 1–3 port to Linux hosts:
- `sandbox.rs` talks to a VMM through the `Vmm` trait that `plan.md` already plans.
- A Firecracker or Cloud Hypervisor backend replaces Virtualization.framework.
- That backend also gets memory snapshots and fork in under 250 ms, which macOS can't do
  today.

That port would be its own spec. Revisit if Apple's licence changes, or if track 3 has
outside demand.

## Rust-first tooling

rekcod is Rust end to end today (host, guest agent, proto), and the platform stays that way
where a mature crate exists. Every new service is a `rekcod` subcommand in the same binary
(or the static musl agent in the guest), so the fleet ships one artifact per OS.

| Need | Use | Instead of |
|---|---|---|
| HTTP API, controller, node | `axum` + `hyper` + `tokio` (already in the tree) | Go or Node services |
| MCP server | `rmcp` (official Rust MCP SDK) | a TypeScript MCP server |
| mTLS, internal CA, join tokens | `rustls` + `rcgen` | OpenSSL, cfssl |
| Ingress proxy (`rekcod edge`) | `pingora` (Cloudflare's proxy framework) | Caddy |
| ACME certificates, wildcard via DNS-01 | `instant-acme` | Caddy's ACME |
| Image registry (`rekcod registry`) | `axum` + `object_store`, OCI distribution spec | `registry:2` (Go) |
| Object storage | RustFS (dev, CI, self-hosted) via `object_store` | MinIO |
| Postgres client and migrations | `sqlx` | an ORM in another language |
| Git clone, diff for workspaces | `gix` (gitoxide); `imara-diff` for `sandbox diff` | shelling out to `git` |
| Guest DNS allowlist | `hickory-proto`/`hickory-server` stub inside `rekcod-agent`, writing to nftables | `dnsmasq --nftset` |
| Guest firewall rules | `rustables` (netlink), or the `nftables` crate over `nft -j` | `iptables` shell in `boot.sh` |
| Metrics | `metrics` + `metrics-exporter-prometheus` | a Go exporter |
| Logs | `tracing` + `tracing-subscriber` | ad hoc `log!` |
| Tarball workspaces | `tar` + `zstd` | `tar` subprocess |
| macOS guests (track 4) | `objc2-virtualization` (already used) | Tart (Swift, FSL) |
| Linux-host backend (if ever hosted) | Firecracker or Cloud Hypervisor, both Rust, behind the `Vmm` trait | QEMU |

Things that stay non-Rust, on purpose:
- `dockerd`, BuildKit and containerd in the guest. Running upstream `dockerd` unmodified is
  rekcod's core promise; replacing it (e.g. with `youki`) breaks API parity.
- Postgres as the controller's store. It's the thing I'd trust to hold the audit log.
- The Linux kernel and busybox userland in the guest.

Crates I haven't used in this repo yet and need a spike before committing to: `pingora`
(its ACME story is separate, hence `instant-acme`), `rustables` (younger than the
JSON-over-`nft` route), and the hickory server pieces for a filtering stub. The registry is
the biggest bet: the OCI distribution API is small, but push/pull edge cases (chunked
uploads, cross-repo mounts) are where a hand-rolled one breaks. If the spike goes badly,
`zot` (Go) is the fallback, not `registry:2`.

## Order of work

1. Track 1, milestones M1–M6 (`agent-sandboxes.md`).
2. Legal read of §2I for an internal fleet.
3. Track 2: node and controller, then auth and quotas, then git workspaces.
4. Track 3: build and registry, then deploy and ingress, then previews.
5. Track 4, only on demand.

## Open questions

- Does §2I cover an internal fleet? This blocks tracks 2–4.
- Controller on Linux or on a Mac? Linux is cheaper and has ECC. A Mac means one less
  machine type to run. I lean Linux.
- Is one `rekcod edge` enough, or does ingress need two for failover? For internal use, one with a
  fast restart is my default.
- Can Tart's image format and tooling be reused for track 4, given its FSL licence?
