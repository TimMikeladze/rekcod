# Agent sandboxes: spec

**Status:** built, 2026-10-12 (M1–M6). Track 1 of [`platform.md`](platform.md). Research: [`paas.md`](paas.md).
"As built" at the end lists where the code differs from this spec.

## What we're building

Throwaway Linux VMs on the user's own Mac that a coding agent creates, runs commands in,
forks and destroys through a CLI, a local HTTP API and an MCP server. Each sandbox is a
full `dockerd` that sees one copy of one project and nothing else on the Mac or the LAN.

This is the "local" row of `paas.md`. A self-hosted fleet and a hosted service are later
phases with their own gates (end of this doc). Nothing in phase 1 needs a server.

### What changes from `rekcod x --sandbox`

| | `rekcod x --sandbox` today | Agent sandbox |
|---|---|---|
| How many | one; a second directory restarts it | many, each with an ID |
| Lifetime | one command, then power off | a session: create, many `exec`, `rm` |
| Project | live mount of the current directory | a copy-on-write copy by default, live mount on request |
| Disk | one persistent `~/.rekcod/sandbox/vm/data.img` | APFS clone of a golden disk, deleted on `rm` |
| Network | internet open, Mac and LAN blocked | same blocks, plus an egress allowlist |
| Caller | a person | an agent, through CLI, HTTP or MCP |
| Record | none | every exec and its result in a log; `diff` of the workspace |

`rekcod x --sandbox` stays. In M2 it becomes "create, exec, rm" on top of this, with
`--mount` to keep today's live-mount behaviour.

## Concepts

- **Sandbox.** One VM plus its state directory `~/.rekcod/sb/<id>/`. The ID is 8 lowercase
  base32 characters. `--name` adds a label; either works anywhere an ID does. The path stays
  short because the docker socket has to fit in `sun_path` (104 bytes): a long `REKCOD_HOME`
  failed at bind in the density test.
- **Golden disk.** A data disk with common images already pulled
  (`~/.rekcod/golden/<name>/data.img`). Every sandbox starts from a `clonefile(2)` of one,
  so creation copies no data and image pulls are skipped. `default` is built on first use.
- **Workspace.** The project directory as the sandbox sees it, at its real Mac path.
  - `copy` (default): `clonefile` of the project into `~/.rekcod/sb/<id>/ws/`, shared into
    the VM as the project path. The agent can't touch the real repo. `diff` and `apply` bring
    changes back.
  - `mount`: the live directory, as `rekcod x --sandbox` does now. Edits land immediately.
- **Workspace container.** A long-running container (`rekcod-ws`) inside the sandbox, with
  the workspace mounted and the image picked the same way `rekcod x` picks one. `exec` runs
  `docker exec` in it, so the toolchain matches the project and `compose up` from inside
  still works through the sandbox's own `dockerd`.

## CLI

Every action is a CLI command first (`docs/frontends.md`). HTTP and MCP call the same
functions.

```sh
rekcod sandbox create [DIR] [--name N] [--mount] [--image IMG] [--golden NAME]
                      [--cpus N] [--memory GiB] [--allow-host H]... [--offline]
                      [--secret NAME]... [--ttl 30m] [--json]
rekcod sandbox exec ID [--workdir P] [--env K=V]... [--timeout 10m] -- CMD...
rekcod sandbox shell ID                     # interactive exec of $SHELL in rekcod-ws
rekcod sandbox cp ID SRC DST                # either side may be ID:/path
rekcod sandbox fork ID [--name N]           # new sandbox from ID's disk + workspace
rekcod sandbox diff ID [--stat]             # workspace vs the project it was copied from
rekcod sandbox apply ID [--check] [--reviewed]  # write the diff into the real project
rekcod sandbox ports ID                     # forwarded ports, as in rekcod status
rekcod sandbox log ID [-f] [--json]         # the exec log
rekcod sandbox ls [--json]
rekcod sandbox stop ID                      # power off, keep state; exec boots it again
rekcod sandbox rm ID... [--all]             # force stop, delete ~/.rekcod/sb/<id>
rekcod sandbox golden build [NAME] [--image IMG]...
rekcod sandbox golden ls | rm NAME
```

`create` prints the ID, or one JSON object with `--json`. `exec` streams stdout and stderr
and exits with the command's exit code. Every command that takes an ID fails with
`no sandbox <id>` and exit code 2 when it doesn't exist.

### Defaults

| Setting | Default | Why |
|---|---|---|
| workspace | `copy` | an injected agent can't wreck the real repo or `.git/hooks` |
| CPUs | 2 | 32 VMs fit on an 18-core M5 Pro alongside the main VM |
| memory | 2 GiB | memory isn't returned to macOS until the VM stops (`phase5.md`) |
| egress | allowlist preset `dev` | see Network |
| TTL | 30 min idle, then `stop` | a stopped sandbox costs disk only |
| max running | 75% of RAM, minus the main VM, divided by each sandbox's memory | `create` fails with the numbers instead of swapping the Mac |

## How it works

### Create

1. Pick the golden disk; build `default` if it's missing.
2. `clonefile` the golden `data.img` to `~/.rekcod/sb/<id>/data.img`. The root image is
   shared read-only from `~/.rekcod/guest/<version>/`. The guest mounts it read-only and
   puts a tmpfs overlay on top, so nothing clones it.
3. In `copy` mode, `clonefile` the project into `ws/`. APFS clones whole directory trees
   in one call. `.git` is included, so `diff` can use git.
4. Spawn `rekcod daemon` with `REKCOD_HOME=~/.rekcod/sb/<id>` and the sandbox flags. Most of
   this is the existing `sandbox_boot` code with the path made per ID.
5. Wait for `dockerd`, then create `rekcod-ws`.
6. Write `meta.json` with the ID, name, project, mode, image, golden disk, allowlist and
   creation time.

Target: create to first `exec` returning takes under 1.5 s p50 with a warm golden disk.
Today that's 0.9 s to boot plus the image pull this design removes.

### Exec

`docker exec` into `rekcod-ws` over the sandbox's socket. Each call appends one line to
`log.jsonl`:

```json
{"ts":"2026-10-12T14:03:11Z","cmd":["npm","test"],"cwd":"/Users/tim/app","exit":1,"ms":4210,"out_bytes":18233}
```

Output isn't stored. The agent has it, and storing it would keep secrets that a command
printed. `--timeout` kills the exec, not the sandbox. A stopped sandbox boots on the next
`exec`.

### Fork

Fork copies disk state, not memory. Virtualization.framework's save/restore isn't usable
with virtio-fs (`plan.md` §10), so running processes don't survive.

1. Ask the guest agent to `sync` and `fsfreeze` the data disk.
2. Pause the VM.
3. `clonefile` `data.img` and `ws/` into a new ID.
4. Resume and thaw.
5. Boot the new sandbox. `rekcod-ws` and any compose services restart from their restart
   policies.

Target: under 2 s from fork to the child accepting `exec`. The parent pauses for well under
a second, because clones don't copy data.

### Diff and apply

`diff` runs `git diff --no-index` between the real project and `ws/` on the Mac side, so it
works when the guest is stopped. It skips VM-local paths (`node_modules`, `.venv`,
`target`). `apply` writes changed, added and removed files into the real project:

- Both freeze `ws/` first (an APFS clone, taken with the workspace container paused), so a
  process still running in the sandbox can't change what's compared or written. `diff`
  keeps its frozen copy in `<sandbox>/reviewed`, and `apply --reviewed` writes exactly that
  copy. Plain `apply` freezes the workspace as it is at that moment.
- Before writing, it refuses if any of those files changed in the real project since the
  copy. Create records kind, size and mtime (ns) of every project file in
  `manifest.json`; a path whose state differs now is a conflict, including a file moved in
  with an old mtime. Each apply updates the manifest for what it wrote.
- Every write goes through a directory fd opened component by component with
  `O_NOFOLLOW` (`safefs.rs`). A symlink on the way makes the write fail, a symlink at the
  destination is replaced rather than written through, and a new symlink must point
  inside the project. Setuid, setgid and sticky bits are dropped.
- It never writes `.git/`. FIFOs, sockets and devices are left out of the comparison.
- `diff` replaces control characters with `^X` text, so escape sequences in the sandbox's
  files can't hide lines from you in a terminal.
- `--check` prints what would change and exits 1 if anything would.

`mount` mode has no diff or apply; they exit with `sandbox <id> uses --mount`.

### Network

The existing sandbox rules stay: Mac and LAN ranges, link-local and IPv6 out of `eth0` are
rejected in `OUTPUT` and `DOCKER-USER`, and there's no `host.docker.internal`.

New: an egress allowlist enforced in the guest.

- A DNS stub in `rekcod-agent` (`hickory-proto`) answers for allowed names only and adds
  each resolved address to an nftables set. It replaces `dnsmasq` in sandboxes (see
  `platform.md`, Rust-first tooling).
- `eth0` output allows only that set, plus DNS to the agent stub. Everything else is
  rejected, which covers direct IP connections too.
- Preset `dev`: npm, PyPI, crates.io, Go proxy, rubygems, Docker Hub, ghcr.io, quay.io,
  github.com and its codeload and objects hosts, plus Debian, Ubuntu and Alpine mirrors.
  The preset lives in `crates/rekcod/src/egress.rs`.
- `--allow-host` adds names, and `*.example.com` works. `--allow-host '*'` opens the
  internet, as today. `--offline` removes `eth0`.
- Denied connections are logged to `net.log` (destination, port, time), so a person can
  see what the agent tried to reach.

Gap I'm accepting: anything on an allowed host can still receive data, for example a gist
on github.com. The allowlist narrows exfiltration; it doesn't stop it.

### Secrets

`--secret NAME` passes `$NAME` from the caller's environment into `rekcod-ws`, as now.
Secrets aren't written to `meta.json` or the log. A forked sandbox gets the same secrets
only if `fork` is given them again. Injecting credentials through a proxy so the agent never
sees them is out of scope here.

### Teardown

`rm` force-stops the VM. Nothing on a cloned disk is worth a clean shutdown, and graceful
shutdown was 3 s of the 4.6 s `rekcod x --sandbox` run. It then deletes
`~/.rekcod/sb/<id>`. `stop` stays graceful, because the disk is kept.

### Supervisor

Each sandbox is its own `rekcod daemon` process, as today. A sandbox list doesn't need a
long-running supervisor. `ls` reads `~/.rekcod/sb/*/meta.json` and checks each PID. TTL is
enforced by each sandbox daemon on itself.

## HTTP API and MCP

`rekcod sandbox serve` listens on `~/.rekcod/run/sandbox.sock` (unix socket, mode 0600) and
optionally on `--listen 127.0.0.1:PORT` with a bearer token from
`~/.rekcod/sandbox-token`. It never listens on other interfaces.

The caller is an agent, so what it may ask for is fixed when the server starts, not taken
from requests:

```sh
rekcod sandbox serve --root ~/src/app        # creates only for ~/src/app and below (default: the current directory)
rekcod mcp --allow-host api.openai.com       # a host callers may add to the allowlist ('*' allows any)
rekcod mcp --secret NPM_TOKEN                # a variable callers may pass in with "secrets"
rekcod mcp --allow-mount                     # lets callers ask for a live read-write share
```

A request outside those gets a 400 that names the flag. `exec` takes `env` only as
`KEY=VALUE`; a bare `KEY` would make docker copy the value from the server's own
environment. The token is checked before the body is read. Request lines and headers are
capped at 16 KiB and 100 headers, bodies at 64 MiB, a peer gets 30 s to send its request,
and 256 connections are served at once.

| Method | Path | CLI |
|---|---|---|
| POST | `/sandboxes` | `create` |
| GET | `/sandboxes` | `ls` |
| GET | `/sandboxes/{id}` | `ls` (one) |
| POST | `/sandboxes/{id}/exec` | `exec`, streamed as NDJSON (`stdout`, `stderr` and `exit` events) |
| GET, PUT | `/sandboxes/{id}/files?path=` | `cp` |
| POST | `/sandboxes/{id}/fork` | `fork` |
| GET | `/sandboxes/{id}/diff` | `diff` |
| POST | `/sandboxes/{id}/apply` | `apply` |
| POST | `/sandboxes/{id}/stop` | `stop` |
| DELETE | `/sandboxes/{id}` | `rm` |

`rekcod mcp` is a stdio MCP server with one tool per row above and the same flags as
`serve`. Claude Code starts MCP servers in the project directory, so `claude mcp add rekcod
-- rekcod mcp` allows that project and nothing else. `sandbox_apply` is the one tool marked
destructive, so MCP clients ask the user first.

E2B-compatible endpoints are a stretch goal. They'd let existing agent SDKs point at
rekcod. I'd only do them once the native API has settled.

## Menu bar

A "Sandboxes" section listing each sandbox with its name, project and state. Actions come
from a new `sandbox_actions` list in `actions.rs`:

| Kind | Command | Asks first |
|---|---|---|
| `SandboxShell` | `rekcod sandbox shell ID` (in Terminal) | no |
| `SandboxDiff` | `rekcod sandbox diff ID` (in Terminal) | no |
| `SandboxApply` | `rekcod sandbox apply ID` | yes |
| `SandboxStop` | `rekcod sandbox stop ID` | no |
| `SandboxRemove` | `rekcod sandbox rm ID` | yes |

Creating a sandbox stays CLI and agent only; there's no menu item for it. Add each `Kind`
to `Kind::ALL`. The three parity tests (`every_kind_is_offered`,
`menu_offers_every_action_kind` and `every_action_is_a_cli_command`) must pass. Update
`docs/frontends.md` and the README in the same change.

## Code layout

| File | What |
|---|---|
| `crates/rekcod/src/sandbox.rs` | IDs, `meta.json`, create, exec, fork, diff, apply, rm, ls; takes `sandbox_boot` out of `xrun.rs` |
| `crates/rekcod/src/golden.rs` | build, list and clone golden disks |
| `crates/rekcod/src/egress.rs` | presets and allowlist to guest config |
| `crates/rekcod/src/sandbox_api.rs` | HTTP server and MCP stdio server over `sandbox.rs` |
| `crates/rekcod-agent` | `fsfreeze`/thaw control messages and nftables egress setup |
| `crates/rekcod-proto` | `Control::Freeze`, `Control::Thaw`, egress config message |
| `guest/boot.sh` | read-only root with a tmpfs overlay for sandboxes, egress rules before `dockerd` |

`clonefile` comes from `libc`; there's no new dependency for it.

## Milestones

Each milestone ships on its own, with tests and docs updated.

1. **Many sandboxes.** IDs, `~/.rekcod/sb/<id>`, `create`, `exec`, `ls`, `stop`, `rm`
   (force stop) and `shell`, still using a mounted workspace and a fresh disk.
2. **Golden disk and copy workspace.** `golden build`, clone on create, `copy` mode, `diff`
   and `apply`. `rekcod x --sandbox` moves onto `sandbox.rs`.
3. **Egress allowlist.** nftables sets, the `dev` preset, `--allow-host` and `net.log`.
4. **Fork.**
5. **API.** `serve`, `mcp` and the log.
6. **Menu bar.** The `sandbox_actions` list, parity tests, `frontends.md`, README.

After M3, `docs/sandbox.md` lists all three of its blockers as addressed except the external
security review. That review gates any claim that this is a security boundary.

## Verification

Add to `scripts/e2e.sh`:
- create, exec, rm round trip; exit code passes through; `rm` leaves no directory.
- 8 sandboxes at once, each sees only its own workspace.
- `copy` mode: the agent deletes a file, the real project still has it; `diff` shows it;
  `apply` removes it; `apply` refuses after a real-project edit to the same file.
- Egress: an allowed registry install works; `curl https://example.com` and a direct IP are
  refused and show up in `net.log`; the Mac and LAN checks from today still pass.
- Fork: a file written before the fork is in the child; a write in the child isn't in the
  parent.

Add `bench/sandbox.sh`, reporting p50 and p95 for:

| Measurement | Gate |
|---|---|
| create to first exec, warm golden | < 1.5 s p50 |
| fork to child exec | < 2 s p50 |
| `rm` | < 0.5 s |
| 16 sandboxes created at once | all ready < 5 s |
| idle footprint per sandbox | < 350 MB |

The 32-VM run in `paas.md` measured an idle footprint of about 280 MB per VM. Unit tests
cover ID parsing, the allowlist to nftables translation, the apply conflict check and
`meta.json` round trips.

## Later phases

### Self-hosted fleet

The same sandboxes, scheduled across a team's own Mac minis:

- A `rekcod fleet agent` on each Mac reports capacity and runs `sandbox.rs`.
- A controller places sandboxes by free RAM.
- The workspace arrives by `git clone` or upload instead of `clonefile`.

**Gate:** someone with legal knowledge reads macOS SLA §2I ("whether for your own
organization or third parties") and says an internal fleet is fine. Don't start before
that.

### Hosted service

Not planned. Revisit only if one of these changes:
- Apple licenses multi-tenant Virtualization.framework use.
- The demand is macOS guests specifically, and dedicated, 24h-minimum, single-tenant Macs
  for CI fit the product.

Linux on Linux hosts is cheaper, has no license problem, and has better snapshots. If we
ever host, it should be there.

## Open questions

- Is a read-only root with a tmpfs overlay enough for `dockerd`, or does `/var/lib/docker`
  have to stay entirely on the cloned data disk? I expect the latter, which is fine.
- Does `fsfreeze` on ext4 under VZ's block device give a clone consistent enough for
  Postgres inside the sandbox to recover through its WAL? Test it with
  `tests/stress/postgres.sh`.
- Can `saveMachineStateTo` work for a guest with no virtio-fs device, for example `copy`
  mode moving files over vsock instead? If it can, forks could keep memory.
- How should golden disks be refreshed? Rebuild on `rekcod` upgrade, plus
  `golden build` on demand, is my default.

## As built

Measured with `bench/sandbox.sh` on the M5 Pro, all gates met:

| Measurement | Gate | Result |
|---|---|---|
| create to first exec, warm golden disk | < 1.5 s | 1.32 s p50 |
| fork to child exec | < 2 s | 1.18 s p50 |
| `rm` | < 0.5 s | 0.12 s |
| 16 created at once | < 5 s | 3.2 s |
| idle footprint | < 350 MB | ~300 MB |

`scripts/e2e-sandbox.sh` (19 checks, called from `e2e.sh`) passes.

Where the code differs from the spec above:

- **Golden disk isn't built on first use.** `create` clones `default` if it exists and starts
  from an empty disk otherwise (the first `exec` then pulls its image). Building it takes
  about 15 s, too long to hide inside a `create`. Run `rekcod sandbox golden build` once.
- **Root image is a per-sandbox APFS clone**, not a read-only mount with a tmpfs overlay.
  The existing `prepare_vm` already clones it, and a clone costs no space.
- **Egress uses the agent's existing DNS stub and per-address iptables rules**, not
  hickory or nftables sets. The stub answers REFUSED for names off the list and logs
  `rekcod-egress: denied NAME` to the console; for allowed names it inserts
  `-d IP -j ACCEPT` into `REKCOD-ALLOW` before handing back the answer. Direct connections
  aren't logged, only lookups. Allowlist matching (`rekcod_proto::host_allowed`) is shared
  between host and guest.
- **Fork pauses the workspace container**, not the VM, and syncs first. dockerd keeps
  running, so the clone is crash-consistent. There's no `fsfreeze` control message.
- **Fork keeps the parent's secrets.** They're in the workspace container's config on the
  cloned disk.
- **HTTP and MCP are hand-written on `std`** (about 400 lines in `sandbox_api.rs`), not
  axum and rmcp. Each request is one thread and `Connection: close`; exec streams NDJSON
  until the command exits. MCP's `exec` returns output when the command finishes.
- **No supervisor process.** The idle TTL is enforced by each sandbox's own daemon, which
  checks `run/last-used`.
- **`rekcod x --sandbox` is unchanged.** It still uses `~/.rekcod/sandbox` and a live mount.
- **`diff` skips `.git`, `node_modules`, `.venv` and `target`**, and compares content, so
  a file touched without changes doesn't show up. `apply`'s conflict check compares each
  path's state with the manifest taken at create (sandboxes made before the manifest fall
  back to mtime against creation).
