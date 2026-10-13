# Phase 5: speed gaps, differentiators, polish

Scope: README "Known gaps" plus `plan.md` §3a, §3b, §3d, §3f, and a menu bar item.
Each piece lists the approach, why that approach, and how it's verified.

## Speed gaps

### 1. Faster first access (cold bind mounts)

Today the daemon warms a bind source after the container is **running**
(`/containers/json` lists running containers only), and nothing is warm after a reboot.

- Reconcile with `/containers/json?all=1` so warming starts at `create`, before `start`.
  Ports still come from running containers only.
- Persist recently warmed bind sources (`~/.rekcod/vm/warm-paths`, most recent first, at
  most 32) and warm them right after boot, so `compose up` after `rekcod start` hits a
  warm cache.
- Verify: `bench/headroom.py --context rekcod` cold phases, and first `cat` of a tree
  after `rekcod stop && rekcod start`.

### 2. VM-local directories via a label (§3d)

`labels: { dev.rekcod.local: "node_modules,.next" }` puts those paths on named volumes
inside the VM, so `npm install` and `rm -rf` never cross into macOS.

- The docker socket proxy parses HTTP/1.1 requests on the client→guest direction
  (Content-Length and chunked bodies), and switches to a raw pipe after any request with
  an `Upgrade` header (attach, exec, BuildKit session). Responses are never parsed.
- On `POST /v*/containers/create` with the label, it adds `HostConfig.Mounts` entries
  `{Type: volume, Source: rekcod-local-<project>-<service>-<path>, Target: <abs path>}`.
  Relative paths resolve against `Config.WorkingDir`. Targets the request already mounts
  are left alone. Volume names are stable, so contents survive container recreation.
- Without compose labels, the volume name uses the container name (`?name=`), else a
  hash of the target.
- Trade-off (documented): the Mac editor doesn't see those dirs.

### 3. Lower change latency

FSEvents already runs with latency 0 and `NoDefer` (notify 8.2). The remaining cost is
the daemon's fixed 10 ms batching window and a new vsock connection per batch.

- Flush as soon as the event queue is momentarily empty (leading edge), instead of
  always waiting 10 ms. Bursts still coalesce, because events that arrive while a batch
  is in flight queue up.
- Reuse one control connection for notify batches.
- Verify with `bench/inotify_rtt.py` (Mac write → container inotify → reply file seen on
  the Mac), before and after.

### 4. Returning memory to macOS

Known: the VZ balloon inflates, but the VM's footprint doesn't drop (`phase2.md`).
This is a bounded experiment, not a promise. Check whether ballooned pages are released
under macOS memory pressure (`memory_pressure`), which would make the balloon worth
enabling by default. Record the result either way.

## Differentiators

### 5. `rekcod snap`: volume snapshots (§3a)

```
rekcod snap save [NAME]       # NAME defaults to the current git branch
rekcod snap restore [NAME]
rekcod snap ls [--json]
rekcod snap rm NAME
```

- Project = the compose project of the current directory: running containers labelled
  `com.docker.compose.project.working_dir=$PWD`, else `-p` / `COMPOSE_PROJECT_NAME`,
  else the normalized directory name (compose's rule).
- Volumes = volumes labelled `com.docker.compose.project=<project>`.
- Save: pause the project's running containers (a crash-consistent point;
  Postgres/MySQL recover through their WAL), copy each volume into a snapshot volume
  `rekcod-snap.<project>.<name>.<volume>` (labelled), then unpause.
  The copy runs in a helper container with `cp -a`.
- Restore: stop the project's running containers, replace each volume's contents
  from the snapshot, start the same containers again.
- Not reflinks: the data disk is ext4. A copy inside the VM runs at disk speed, which
  still beats migrate + seed. btrfs/XFS reflinks are a later optimization.
- Host-only: works on stock dockerd through the docker API; no guest change.
- Verify: Postgres compose stack → seed → save → drop table → restore → data back;
  time save/restore vs `up` + seed.

### 6. `rekcod x <cmd>`: run any command in a container, no Dockerfile (§3b)

`rekcod run` is docker's (passthrough), so the zero-config runner is `rekcod x`.

- Detect the image from, in order: `.tool-versions`/`mise.toml`, `.nvmrc`/
  `.node-version`/`package.json` `engines.node`, `.bun-version`/`bun.lock`,
  `.python-version`/`pyproject.toml`, `go.mod`, `Cargo.toml`, `Gemfile`, `deno.json`,
  then the command name (`node`, `npm`, `pnpm`, `bun`, `python`, `uv`, `go`, `cargo`, ...).
  `--image` overrides.
- Run: `docker run --rm` with the cwd bind-mounted at the same path, `-w $PWD`,
  `-it` when stdin is a TTY, `.env` passed as `--env-file`. Exit code passes through.
  pnpm/yarn go through corepack.
- Linux deps live on a VM-local volume keyed by project path (`node_modules`,
  `.venv`), so Mac-built native modules never load on Linux and installs are fast.
- Ports: the container uses the VM's host network. The agent reports TCP listeners
  owned by the container's processes (new `listeners <container id>` control command),
  and the daemon forwards each to the same port on the Mac's `localhost` through the
  existing FORWARD_PORT path. Containers opt in with the label `dev.rekcod.autoports`.
- `-p PORT` publishes explicitly when auto-detection isn't wanted (`--no-autoports`).

### 7. `rekcod x --sandbox`: a separate VM that sees only the project

Threat model: `docs/sandbox.md`. Summary:

- A second VM (`~/.rekcod/sandbox`, own data disk, MAC address and sockets) whose only
  Mac share is the project directory, mounted at its real path
  (kernel cmdline `rekcod.project=<path>`).
- No `host.docker.internal` splice to the Mac's loopback (the reverse pool is off), so
  sandboxed code can't reach Mac-local services. `--offline` adds `--network none`.
- Secrets go in only through `--secret NAME` (copied from the Mac environment).
- The sandbox VM powers off when the command exits (`--keep` leaves it up). Its data
  disk persists, so images are cached.
- Not claimed: egress allowlisting, protection against VZ escapes. This is a security
  claim, so it needs external review before being marketed as one.

### 8. Scriptable API (§3f)

`rekcod status --json`, `rekcod snap ls --json`. Status JSON is the daemon's
`status.json` plus versions and stats; it's the contract the menu bar item uses.

## Polish

### 9. Menu bar item

`rekcod menubar`: an `NSStatusItem` (objc2-app-kit, same binary) showing running /
stopped, with Start, Stop, forwarded ports (click opens `http://localhost:<port>`),
open logs, and Quit. Polls `status.json` every 2 s. `rekcod menubar --install` adds a
LaunchAgent so it starts at login; `--uninstall` removes it.

### 10. Richer `rekcod status`

New agent `stats` command: virtio-fs host requests by opcode (cache misses), warming in
progress, and data-disk usage inside the VM. `rekcod status` shows the data disk (used in
the VM vs taking up on the Mac), warming, and file-cache misses since boot.

## Guest changes (need a guest rebuild)

The agent gets `listeners` and `stats`; `boot.sh` gets `rekcod.project=`. Everything
else is host-only. The protocol stays backwards compatible: an old guest answers unknown
commands with `error:`, and the host degrades (no autoports, no stats).

## Verification

- `cargo test --workspace`, `cargo clippy`.
- `scripts/e2e.sh` with new checks: label volumes, `snap` save/restore round trip,
  `rekcod x` exit code and autoport, `--sandbox` can't read `~/.ssh`, `status --json`.
- `bench/inotify_rtt.py` and `bench/cold_start.sh` before/after numbers in this file.
- Menu bar: a screenshot of the running item.

## Results (2026-10-12, M5 Pro, macOS 27)

**Status:** all ten items built. `scripts/e2e.sh` passes 44/44 (47 with
`E2E_IMPORT_FROM`), 37 unit tests on the Mac (32 host, 5 protocol), 3 agent tests (run on Linux with
`rekcod x cargo test -p rekcod-agent`), clippy clean.

| Item | Result |
|---|---|
| 1. First access | Warming starts at `create`; the 8 most recent bind sources are warmed at boot. `bench/cold_start.sh` (19.5k files, walk 3 s after `rekcod start`): **34 ms** warmed at boot vs 286 ms not (warm repeat: 35 ms). |
| 2. VM-local dirs | `dev.rekcod.local` label works through compose and `docker run`; attach/exec/BuildKit sessions still pass (full e2e green through the new proxy). |
| 3. Change latency | `bench/inotify_rtt.py`, 40 round trips: p50 **26.1 → 13.8 ms**, p90 27.0 → 14.6 ms. OrbStack (phase 0): 12.4 ms. |
| 4. Memory | **Still not returned.** With `REKCOD_BALLOON=1`, after 5 GB was touched and freed in the guest, the balloon capped the guest at 1.6 GB, but the VZ process footprint stayed at 7.2 GB. Under `memory_pressure -l warn` its RSS fell to 5.2 GB (compressed), footprint unchanged. The balloon stays opt-in. Zeroing freed pages (`init_on_free=1`) is worse: the guest touches all 8 GiB at boot. Stopping the VM is what returns memory (`fleet-compute.md`). |
| 5. Snapshots | Postgres with 2M rows (361 MB data dir): save 0.5 s, restore 0.5 s (0.3 s after `down -v`), data back, compose adopts the recreated volume. Restore waits for `docker stop`, so a container that ignores SIGTERM costs its 10 s timeout. |
| 6. `rekcod x` | Detection unit-tested per runtime. Warm `rekcod x node -v`: ~75 ms vs ~10 ms native (gate: ≤300 ms over native). A server bound to the container's `127.0.0.1:3999` answered on the Mac's `localhost:3999` with no `-p`; the forward closed when the container stopped. |
| 7. Sandbox | Verified as in `sandbox.md`. Second VM boots in ~1 s. |
| 8. API | `rekcod status --json`, `rekcod snap ls --json`. |
| 9. Menu bar | Built; entries checked through the accessibility API (no screenshot: the terminal lacks Screen Recording permission). Start/Stop clicks and `--install` not exercised. |
| 10. Status | Disk used in the VM vs on the Mac, file-cache trips to macOS by opcode, warming in progress, sandbox VM and its ports. |


Naming: the runner is `rekcod x`, because `rekcod run` already means `docker run`.
