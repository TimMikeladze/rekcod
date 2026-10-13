# rekcod as a platform: hosted PaaS, agent sandboxes, or neither

**Status:** research, 2026-10-12. Nothing here is built yet, apart from the sandbox teardown
fix below.

## Short answer

Don't build a hosted PaaS on Mac hardware. Build **local agent sandboxes**: many throwaway
VMs on the developer's own Mac, driven by an API that coding agents call. rekcod is most of
the way there already, and it's the one version of this where a Mac is an advantage rather
than a cost.

| Option | Verdict | Why |
|---|---|---|
| Hosted Linux PaaS or sandboxes on rented Macs | no | Crowded market at $0.05/vCPU-hr on Firecracker, Macs cost more per core than Hetzner ARM, and the macOS license likely forbids it (below) |
| Hosted macOS sandboxes for agents | no, for now | Real gap, but Apple's license only allows CI, 24h minimum leases, one tenant per Mac, two macOS VMs per host. rekcod's Linux work doesn't help macOS guests |
| Local sandboxes for agents on the user's own Mac | **build** | No license problem, no per-hour cost, the repo is local so bind mounts are fast, and measured density is already good |
| Self-hosted fleet on a team's own Mac minis | later | Same code as local, plus a scheduler. Check the license (§2I) first |

## What I measured

M5 Pro, 18 cores, 48 GB, macOS 27, release build of `main`. The user's main VM (6 CPUs,
8 GiB) stayed up throughout.

| | Result |
|---|---|
| `rekcod x --sandbox ... true`, end to end, before the fix | 46 s |
| same, after the fix | 4.6 s (0.9 s boot, ~0.7 s run, ~3 s graceful guest shutdown) |
| 8 VMs (2 CPU, 1 GiB) started together | all ready in 0.9–1.4 s |
| 24 more (1 CPU, 1 GiB) started together, 32 running | all ready within 3.96 s, p50 2.9 s |
| footprint of 32 VMs, 8 of which had run Python | 10.7 GB total, about 280 MB idle each |
| `python:3.13-slim` first run, 8 VMs pulling at once | 3.7–10 s (each VM pulls its own copy) |
| same, warm, 8 at once | 0.36 s each |
| stopping all 32 together | 3.3 s |

The VMs were plain `rekcod start` with a separate `REKCOD_HOME` each. One gotcha: the docker
socket path has to fit in `sun_path` (104 bytes), so a long `REKCOD_HOME` fails at bind.

### The fix

`rekcod x --sandbox` spawns the sandbox daemon as its own child. After the guest powered off,
the daemon exited but stayed a zombie, `kill(pid, 0)` kept returning 0, and `stop_daemon`
waited out its whole 45 s deadline. It now reaps the child with `waitpid(WNOHANG)`
(`crates/rekcod/src/cli.rs`).

## Why not a hosted PaaS

### The license

The macOS 27 license has the same terms as Tahoe 26:

- §2B(iii): at most two extra macOS copies in VMs per Mac, for development, testing, or
  personal non-commercial use. Virtualization.framework refuses a third macOS guest.
- §2I: no using macOS "or any of its functionality, in connection with service bureau,
  time-sharing, terminal sharing or other similar types of services".
- §3: leasing is allowed only for CI ("Permitted Developer Services"), for at least 24
  consecutive hours, with one lessee having sole use of the Mac, and Apple has to be told
  in advance.

Linux guests aren't capped by the framework. But a paid, multi-tenant service where
Virtualization.framework is the hypervisor reads to me like the §2I service bureau case even
when every guest is Linux. I'm not a lawyer. Anyone hosting this for strangers needs one
before writing code.

### The market

Every hosted agent sandbox I checked is Linux on Linux hosts:

| | Isolation | Start | $/vCPU-hr | $/GB-hr |
|---|---|---|---|---|
| E2B | Firecracker | under 200 ms, pause/resume | 0.0504 | 0.0162 |
| Daytona | microVM | "sub 90 ms", fork | 0.0504 | 0.0162 |
| Vercel Sandbox | Firecracker | not published | 0.128 active CPU | 0.0212 |
| Cloudflare Containers | VM per instance | not published | 0.072 active CPU | 0.009 |
| Modal | gVisor | not published | ~0.071 | 0.024 |
| Morph | VM snapshot/branch | under 250 ms branch | MCU bundles | |

E2B and Daytona raised Series As in 2025 and 2026. Modal raised $355M in 2026. They snapshot
and fork running VMs in hundreds of milliseconds. rekcod can't: Virtualization.framework's
save/restore has been reported failing for Linux guests (unverified), and virtio-fs state
blocks restore anyway (`docs/plan.md` §10). A 1 s cold boot is close to rekcod's floor.

### The hardware

My arithmetic, not sourced: a 32 GB Mac mini M6 over 36 months plus $50/mo colo is about
$86/mo, around $0.01 per core-hour, and half those cores are efficiency cores. A Hetzner
CAX41 (16 ARM vCPU, 32 GB) is €41/mo. Mac minis also have no ECC, no BMC and one power
supply. So Macs win only when the workload needs macOS, and that's the workload the license
restricts most.

### macOS sandboxes specifically

Nobody sells self-serve hourly macOS sandboxes with public pricing. Cua has macOS on a
waitlist. Cirrus Labs joined OpenAI in April 2026 and Tart is community-maintained now. The
gap is real, but the license explains it: one customer per Mac for at least 24 hours is
MacStadium's or Scaleway's business ($149/mo per M4 mini), not a sandbox API.

## Why local agent sandboxes

Agents run on developers' Macs today (Claude Code, Codex, Cursor) and execute shell commands
with the user's full permissions. Shai-Hulud showed what one bad `npm install` takes with it.
Hosted sandboxes fix that by moving the code to the cloud, which means uploading the repo,
paying per hour and waiting on the network.

Locally:

- There's no license question, since it's the user's own development use of their own Mac.
- It costs nothing per hour, so an agent can keep 10 sandboxes around while it tries
  approaches in parallel.
- The repo stays on the Mac. Bind mounts are rekcod's strongest feature (3–9× faster
  than OrbStack warm), which matters to an agent that runs the test suite 50 times.
- Each sandbox is a full `dockerd`, so `compose up` with Postgres works inside it. Apple's
  `container` 1.0 gives one VM per container and no compose, and the Firecracker clouds give
  you a Linux box, not a docker host.
- It works offline and on a plane.

Apple `container` is the closest thing. It's Apache-2.0, a VM per container on
Virtualization.framework, and starts in about 0.9 s in one benchmark. It doesn't do compose,
cached bind mounts, or a firewall that keeps the guest off the Mac and LAN.

## What's missing

In order of what blocks an agent first:

1. **More than one sandbox.** `rekcod x --sandbox` runs one at a time and restarts it for
   another directory. The density test shows the VMs themselves are fine. What's needed is a
   sandbox ID in the path (`~/.rekcod/sandboxes/<id>`, kept short for `sun_path`) and a
   per-sandbox MAC and vsock set.
2. **A CoW golden disk.** Each VM pulls its own images, 3.7–10 s for a Python image with 8
   pulling together. Keep one `data.img` with common images pre-pulled and `clonefile(2)`
   it per sandbox. APFS clones are instant and copy on write, so this also gives the
   per-run disposable disk that `docs/sandbox.md` lists as a blocker for calling the
   sandbox a security feature.
3. **An API agents call.** Create, exec, read/write file, ports, destroy. Expose it as a CLI
   first (`rekcod sandbox create|exec|rm|ls`, per `docs/frontends.md`), then a local HTTP
   API and an MCP server on top. An E2B-shaped API would let existing agent SDKs point at
   it.
4. **Egress allowlist** (`--allow-host`), enforced in the guest. Without it an agent can
   still exfiltrate the project over the internet.
5. **Fast teardown.** Graceful shutdown is about 3 s of the 4.6 s. A disposable clone can
   be force-stopped, since nothing on its disk is kept.
6. **External security review** of the share setup and firewall rules (`docs/sandbox.md`).

Memory never going back to macOS (`docs/phase5.md`) matters less here: a disposable VM
returns everything when it stops. It does cap long-lived sandboxes at their configured size.

## Open questions

- Does §2I cover an internal Mac mini fleet that a company runs for its own engineers? The
  clause says "whether for your own organization or third parties". This decides whether
  the self-hosted fleet is worth building.
- Can Virtualization.framework's `saveMachineStateTo` work for a guest without virtio-fs
  (a sandbox that gets its files by copy instead)? If it can, fork in under a second is
  possible.
- Nested virt works on M3+ for Linux guests. Is Firecracker inside the rekcod VM fast enough
  to give per-sandbox snapshots without paying for a full VZ VM each?

Spec for the recommended path: [`agent-sandboxes.md`](agent-sandboxes.md).
