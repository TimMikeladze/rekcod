# Production readiness

Audit of `main` at f994023 (2026-10-16). Four read-only passes: the local VM, the fleet
(controller, nodes, edge, RustFS), the sandboxes, and release/dashboard/tests. Nothing was
changed and the VM wasn't touched. The items marked **verified** were confirmed by reading the
exact code path and, for the git injection and the `apply` symlink write, by running the same
shape in a scratch directory. Everything else is from reading code; "unverified" means the
reasoning holds but nobody ran it.

Test state: `cargo test --workspace` passes 87 (rekcod) + 5 (proto). The agent's 6 tests are
`cfg(target_os = "linux")` and never run anywhere, CI included. Dashboard: 28 vitest pass,
typecheck and lint clean, `npm audit --omit=dev` reports 7 high (all via `shadcn` in
`dependencies`). e2e only runs on a developer Mac.

## Status after the 0.3.0 work

What changed since this audit, item by item, is in `CHANGELOG.md` (Unreleased) and the
commits after f994023. In short:

- Section 1: all fixed. S7: sandbox egress is now decided by the daemon on the Mac (smoltcp, SNI/Host checks), so guest root can't bypass it (`docs/sandbox-network.md`).
- Sections 2 and 3: all fixed, except the per-mount `strict` cache mode (not built) and
  memory return to macOS (unchanged, a Virtualization.framework limit).
- Sections 4 and 5: all fixed. App traffic between the edge and nodes stays plain TCP;
  nodes belong on a tailnet.
- Section 6: fixed, except the shared `--sandbox` VM and cache volumes across projects
  (documented in `docs/sandbox.md`) and port forwards from sandboxes to the Mac's
  localhost (they no longer shadow anything already listening).
- Section 7: everything that doesn't need an account or a decision. Waiting on you:
  a Developer ID (notarization is wired and turns on with the secrets in
  `docs/releasing.md`), a self-hosted Mac runner for e2e (`REKCOD_E2E_RUNNER`), the v0.3.0
  tag, publishing the Homebrew tap, and turning on GitHub private vulnerability reporting.
- Section 8: fixed (log rotation and trimming, `rekcod doctor`, metrics, alerts,
  `--backup-dir`, cleanup).
- Section 9: request-level controller tests, the `Conn` spill test, `safefs` tests and
  `scripts/e2e-lifecycle.sh` (VM crashes, damaged disks, repair) were added.
- An external security review of the sandboxes is still not done; `docs/security-review.md`
  is the brief. An internal adversarial pass (three reviewers, real VMs, guest root with the
  guest firewall flushed) confirmed the sandbox, apply, API and dashboard claims and found
  four things, all fixed: node certificates came from the CA clients pin for the controller,
  app instance sandboxes were reachable as sandboxes, the main VM's `--network user` could
  reach the Mac's loopback, and `serve`/`mcp` accepted git sources that skipped `--root`.
  A role-matrix test now covers every fleet write route against every caller kind, and
  `scripts/e2e-fleet.sh` checks cross-tenant access live over https against real
  instances (87 checks).
- Verified on this Mac with isolated homes: `scripts/e2e-lifecycle.sh` 21/21, `scripts/e2e-sandbox.sh` 26/26, `scripts/e2e-fleet.sh` 87/87, `cargo test --workspace` 121 + 5, agent tests 7 in a Linux container. `scripts/e2e.sh` 70 checks on the main VM after it moved to 0.3.0.

## Where it stands

The local Docker replacement is close for a single developer who knows to keep backups. Three
things can lose data without warning (data disk reformat, silent stream truncation, snapshot
save/restore ordering), and the shipped release is 25 commits behind the README.

The fleet is not ready to host anything public. Any token holder can run commands on the
controller and node Macs, reach other tenants' sandboxes, and clone the node owner's Docker
disk. It also doesn't recover by itself from a node blip or reboot.

The sandboxes don't hold against hostile code yet. Egress filtering runs inside the guest,
`apply` writes through symlinks, and API/MCP callers pick their own policy. `docs/sandbox.md`
already says this isn't a security boundary; the README says more than that.

## 1. Exploitable today

| # | What | Where | Scenario |
|---|---|---|---|
| S1 | Git argument injection (**verified**) | `fleet.rs:227`, `fleet.rs:1822`; reached from `upsert_app` `fleet.rs:2323`, deploy `fleet.rs:2434`, jobs `fleet.rs:642`, `sandbox_api.rs:63` | `ref = "--upload-pack=sh -c '…'"` runs a command as the controller or node user, outside any VM. Reachable from the local sandbox socket and MCP too. Every token is a member of `default` (`fleet.rs:1038`), viewers included |
| S2 | Query string overrides sandbox id (**verified**) | `sandbox_api.rs:230-233`, controller forwards query `fleet.rs:1250` | `POST /sandboxes/<mine>/exec?id=<victim>` runs in someone else's sandbox or app instance, across namespaces |
| S3 | Controller forwards the raw create body to nodes | `fleet.rs:1600`, `sandbox_api.rs:73,78`, `sandbox.rs:124,348` | `"golden": ".."` clones the node owner's `~/.rekcod/vm/data.img` into the attacker's sandbox; `"secrets": ["GITHUB_TOKEN"]` copies the node daemon's env in |
| S4 | Any container can drive the dashboard (**verified**) | `daemon.rs:879-919` splices `host.docker.internal:N` to Mac `127.0.0.1:N`; `dashboard/src/lib/guard.ts` only checks headers a non-browser client sets | A postinstall in `docker run node npm install` scans 4500-4699, sends `Host: localhost:4500` and `x-rekcod: 1`, then `POST /api/run {"argv":["app","destroy","prod"]}`. Confirms are UI-only |
| S5 | `sandbox apply` writes through symlinks (**verified**) | `sandbox.rs:771-775` (`std::fs::copy` follows a symlink at `dst`), `create_dir_all` follows symlinked parents | Apply 1 writes `x -> ~/.zshrc`, apply 2 replaces it with a file; the conflict check (`sandbox.rs:741`) misses it. Ends as code execution on the Mac |
| S6 | API/MCP caller sets its own sandbox policy | `sandbox_api.rs:66-80` | `dir: $HOME`, `mount: true`, `allow_hosts: ["*"]`, any `secrets` name. An agent given MCP can mount your home read-write |
| S7 | Egress filter enforced in the guest | `boot.sh:96-124`, `agent.rs:536`; container runs `--network host` with default caps (`sandbox.rs:233`) | `NET_RAW` raw sockets skip iptables, reaching the Mac gateway and LAN (unverified, `CONFIG_PACKET=y`) |
| S8 | `exec` env without `=` | `sandbox.rs:428-430` | `env: ["AWS_SECRET_ACCESS_KEY"]` makes the docker CLI copy rekcod's own env value in, bypassing `--secret` |

Fix directions: reject `-`-prefixed refs/URLs, allow https/ssh only, put `--` before
positionals, `protocol.file.allow=never` (S1). Apply query params before the path id or never
let them set `id` (S2). Whitelist create fields on the controller and validate `golden` as
`[a-z0-9-]+` (S3, also locally). Per-launch dashboard token in a 0600 file, required on every
`/api` route as a SameSite=Strict cookie, and `reverse_pool` refuses the dashboard's port (S4).
Open destinations with `O_NOFOLLOW` under a project dir fd, refuse symlinks that leave the
project, apply from a frozen snapshot of `ws/` (S5). Fix the policy at `serve`/`mcp` start
(S6). `--cap-drop ALL`, no host network, long term enforce on the Mac side (S7). Require
`K=V` (S8).

## 2. Data loss

| # | What | Where |
|---|---|---|
| D1 | A data disk `blkid` doesn't recognise gets `mkfs.ext4`'d with no prompt (**verified**). A failed mount only logs, so dockerd writes to the 64 MB root disk that upgrades replace | `guest/rootfs/etc/rekcod/boot.sh:36-40` |
| D2 | A reader 64 MiB behind gets a clean EOF, not an error (**verified**). `docker logs`, `cp`, `save`, `snap save --push`, `import` can truncate silently | `vm.rs:368-380` |
| D3 | `snap save` deletes the old snapshot before the new copy succeeds; `restore` wipes live volumes with no backup; Ctrl-C leaves containers paused; `feature/x` and `feature-x` collide | `snap.rs:258`, `:163`, `:289-298` |
| D4 | Guest flush maps to `fsync`, not `F_FULLFSYNC`. A power cut can corrupt the ext4 journal, which then feeds D1. README claims native-Mac durability | `vm.rs:231-237`, `README.md:95` |
| D5 | Missing `controller/secret-key` is silently regenerated. After a restore on another Mac every sealed secret and the storage keys are unreadable | `fleet.rs:3075-3083` |

Fix directions: never mkfs a disk with data, `e2fsck -p` at boot, refuse to start dockerd if
the mount fails (D1). Spill past the cap or return `ConnectionReset` (D2). Copy to a temp
volume then rename, auto-snapshot before restore, signal guard that unpauses (D3). Document
it and fsck at boot (D4). Refuse to create a key when the DB has sealed values (D5).

## 3. The local daemon under real conditions

- **Stale pid file.** `cli.rs:32-35` trusts `kill(pid, 0)`. After a crash or reboot the pid is
  reused: `start` says "already running" and `stop` SIGTERMs an unrelated process
  (`cli.rs:200`). Hold an `flock` on the pid file.
- **Concurrent boots.** No lock across check/spawn/wait (`cli.rs:125-168`). Two `rekcod ps`
  after a reboot spawn two daemons that steal each other's sockets and possibly mount the same
  `data.img` (unverified whether VZ locks it). Same race in the guest download
  (`fetch.rs:45-50`).
- **fd limit 256 under launchd** and accept loops that `continue` without sleeping
  (`daemon.rs:293,501,857`). Around 120 concurrent forwarded connections pins a core and
  refuses new ones. The guest agent's `accept().await?` (`agent.rs:60`) drops every session on
  one EMFILE. Raise `RLIMIT_NOFILE`, back off on accept errors.
- **No supervisor or start at login** for the VM (`service.rs` only covers fleet services).
  After a reboot the context and `/var/run/docker.sock` point at a dead socket and
  `restart: always` containers don't come back.
- **Mac port shadowing.** `SO_REUSEADDR` at `daemon.rs:773` lets a `-p 127.0.0.1:5432` bind
  while a Mac Postgres holds `*:5432` (verified on this machine), contrary to the README.
- **Dropped FSEvents batches** are only logged (`daemon.rs:395-406`). With day-long TTLs,
  containers read stale files. Re-queue the roots as tree invalidations.
- **Menu bar freezes.** `send_control` has no read timeout and `Stats` waits up to 600 s
  (`daemon.rs:302-321`); `fill()` runs on the AppKit main thread and also makes fleet HTTP
  calls (`menubar.rs:361`).
- **Sockets inherit umask.** `docker.sock`, `control.sock` and `run/` are 0755 here
  (`daemon.rs:142,179`, `paths.rs:26`). Anyone in `staff` with umask 002 gets root dockerd.
- Smaller: shutdown timeouts overlap (host 20 s vs guest 18 s + dockerd 15 s,
  `daemon.rs:246`, `shutdown.sh:6`); boot flags aren't persisted so passthrough boots use
  defaults (`cli.rs:481`); clock resync only every 30 s, nothing on wake (`daemon.rs:412`);
  split-DNS (`/etc/resolver/*`, MagicDNS) ignored (`dns.rs`); `data.img` not excluded from
  Time Machine; no free-space check on the Mac; `lock().unwrap()` everywhere with unwind
  panics; `vm.state()` blocks a tokio worker every 500 ms (`daemon.rs:220`); no
  `stop --force`.

## 4. The fleet doesn't converge

- **Lost nodes never come back.** After 15 s without a heartbeat instances go `lost`
  (`fleet.rs:1220`); the node returns to `ready` but its instances never do, and `routes()`
  drops them (`fleet.rs:1768`). A Wi-Fi blip, sleep or a controller restart takes apps down
  until someone redeploys. The first watchdog tick after a restart races the heartbeats.
- **No reconcile after a node reboot.** `node_serve` restarts relays, not VMs
  (`fleet.rs:743`). Heartbeats carry the sandbox list; the handler ignores it
  (`fleet.rs:1315`). The edge serves 502.
- **Concurrent deploys aren't serialized** (`fleet.rs:1913`). A webhook `synchronize` during a
  manual deploy can leave the older commit live; release numbers race on the primary key.
- **Crash mid-deploy** leaves the release `building` forever, which also stops `recycle` for
  that app (`fleet.rs:2200`), and orphans `draining` VMs (`fleet.rs:2055`).
- **Jobs lost on node restart** count against quota forever and their schedule never fires
  again (`fleet.rs:609-637`, `2637`).
- **Multi-Mac setup from the README doesn't work.** `--listen 0.0.0.0:7701` makes endpoints
  `0.0.0.0:port` (`fleet.rs:556`, `740`). e2e only uses `127.0.0.1`.
- **Hung peers stall everything.** Outbound calls have a connect timeout and no read timeout
  (`fleet.rs:57-79`). One hung node blocks the single watchdog thread: lost-node detection,
  backups, idle sleep, preview cleanup.
- **One panic poisons the DB mutex** and every later request panics while the process stays up,
  so KeepAlive never restarts it (`fleet.rs:883`). Migrations swallow errors and have no
  `user_version` (`fleet.rs:895`).

## 5. Fleet security beyond section 1

- Plain HTTP on every hop, carrying tokens, unsealed app secrets in the deploy spec
  (`fleet.rs:1942`) and RustFS root keys (`fleet.rs:2904`). README binds the controller to
  `0.0.0.0`.
- RustFS root keys go to every node on every deploy (`fleet.rs:2896`). One node owns all
  buckets and backups.
- Join token never rotates, compared with `!=`, no node approval or removal, placement trusts
  the node's self-reported `mem_free` (`fleet.rs:1062`, `1294`, `1315`). A rogue node draws
  every deploy and its secrets.
- Edge uses the admin token (`edge/mod.rs:122`). Taking the edge Mac is fleet admin.
- Tokens stored in plaintext, no revoke or expiry (`fleet.rs:815`, `1403-1430`), copied into
  48 hourly backups.
- Hand-rolled HTTP servers: no header caps, 64 MiB body read before auth, no timeouts, a
  thread per connection (`sandbox_api.rs:174-205`, `fleet.rs:3126`). Register, heartbeat and
  webhook take no user token.
- Fork PRs auto-deploy with the base app's env and full egress (`fleet.rs:2815-2818`, `517`).
- A user-set `S3_ENDPOINT` becomes the instance's `reach` and opens a path to the LAN and node
  API (`fleet.rs:519`).
- Public traffic to a sleeping app triggers a wake per request, falling back to a rebuild
  (`edge/mod.rs:145`, `fleet.rs:2148`). Single-flight it.
- Loopback trusted for `X-Forwarded-For` by default with leftmost fallback; anything other than
  same-Mac cloudflared lets clients pick their IP and push fleet-wide bans. IPv6 limited per
  /128.
- Viewers see env values (`fleet.rs:2280`). Webhook has no delivery-id replay check, `closed`
  destroys any app named `<base>-pr-N`, and `GITHUB_TOKEN` sits on curl's argv
  (`fleet.rs:2828`).
- Non-constant-time compares at `fleet.rs:1294`, `1312`, `sandbox_api.rs:304`
  (`subtle_eq::ct_eq` already exists at `fleet.rs:3056`).

## 6. Sandbox claims beyond section 1

- `x --sandbox` and `--mount` share live `.git` read-write (`xrun.rs:257`). The sandbox sets
  `core.fsmonitor`, your prompt's next `git status` runs it on the Mac.
- `diff` and `apply` recompute against a running guest (`sandbox.rs:753`); what's applied may
  not be what was reviewed. `diff` prints guest bytes raw, so escape sequences can hide lines.
- The `dev` allowlist is IP-based on shared CDN addresses and never expires
  (`agent.rs:530-540`), and it's always included. Likely lets other sites on Cloudflare,
  Fastly and AWS through (unverified).
- DNS stub parses only the first question up to the first compression pointer but forwards the
  raw packet (`agent.rs:581`, `daemon.rs:465`). Possible allowlist bypass (unverified).
- macOS sandboxes: NAT only, Mac and LAN reachable, `--offline`/`--allow-host` silently
  ignored (`macos.rs:176`); `StrictHostKeyChecking=no` and one shared key (`macos.rs:415`);
  guest-controlled rsync sender on every diff (`macos.rs:487`).
- No per-sandbox disk quota or free-space check; `x --sandbox` never sets a TTL
  (`xrun.rs:353`); booting a stopped sandbox skips the memory budget (`sandbox.rs:192`);
  `--sandbox` shares one VM and cache volumes across projects.
- Conflict check uses second-resolution mtimes against `created`; a file moved in with an old
  mtime gets deleted without warning (`sandbox.rs:741`).

## 7. Shipping and upgrades

- **The latest release is v0.2.1**, 25 commits behind main and before the breaking
  `refactor(fleet)!`. `Cargo.toml` still says 0.2.1, so `releases/latest` in the README installs
  a binary with none of snapshots, `x`, menu bar, sandboxes, fleet or dashboard.
- Not notarized, ad-hoc signed (`package.sh:28`). Install tells users to strip quarantine and
  doesn't check `SHA256SUMS`.
- `rekcod dashboard` fails on a release install: `release.yml:45` doesn't pass
  `--with-dashboard`, and it needs Node.
- No `rekcod uninstall`. Deleting `~/.rekcod` leaves `/var/run/docker.sock` pointing nowhere,
  the `rekcod` context selected, and KeepAlive agents relaunching a missing binary.
- No `rekcod upgrade`, no brew tap, no version in `status.json`, no host/guest/daemon version
  check, no fleet `X-Rekcod-Version`, old guests never pruned. After replacing the binary the
  old daemon keeps running the old guest. LaunchAgents hard-code the canonical binary path,
  which a brew Cellar path breaks on every upgrade.
- Guest supply chain: docker tgz and kernel tarball downloaded with no checksum
  (`guest/build.sh:82,118,154`), `apk add` unpinned, kernel cache key ignores the `linux-virt`
  version so security fixes don't land. No Dependabot, cargo-deny or npm audit in CI. Actions
  pinned by tag, toolchain floats, `contents: write` on the whole release workflow.
- `release.yml` runs no tests and doesn't depend on `ci`; e2e never runs in CI.

## 8. Operations

- No log rotation anywhere (`cli.rs:139`, `vm.rs:316`, `service.rs:85`, `dashboard.rs:118`);
  `daemon.log` is 6 MB after 5 days here. dockerd logs to the small root disk.
- No `rekcod doctor`/diagnose bundle, no crash reporting, one debug env var for log level.
- Fleet: no metrics, no alerts on lost node or failed deploy, app logs are the last 200 lines of
  live instances and vanish on redeploy, no SIGTERM handling.
- Backups go to the same RustFS, which on a spare is the same disk. Restore brings back stale
  `live` instances with no reconcile.
- Unbounded growth: node `remote-ws` clones and `jobs/`, every release image and build cache in
  RustFS (destroyed previews included), the `audit` and `jobs` tables.
- Edge route table empties on a bad parse (`edge/mod.rs:128`) and isn't persisted, so an edge
  restarted while the controller is down serves nothing.

## 9. Tests that would have caught the above

- No unit tests: `daemon.rs`, `vm.rs` (including the `Conn` overflow), `storage.rs`,
  `dashboard.rs`, `docker.rs`, `cli_fetch.rs`, `edge/idle.rs`. Thin: `fleet.rs` 7 tests for 3316
  lines, `sandbox.rs` 4, `sandbox_api.rs` 2, `guard.ts` none.
- Nothing tests the controller over HTTP: auth matrix, `/ns/NS/` routing, query forwarding,
  create-body passthrough, git input validation, concurrent deploys, lost-node recovery,
  controller restart.
- e2e lacks: data surviving stop/start, `kill -9` of the daemon, stale pid, upgrade keeping
  data, concurrent start, full disk, sleep/wake, multi-host fleet.
- `scripts/e2e.sh:128` removes any `rekcod x` container and `:150` runs `rekcod stop`
  unconditionally, which deletes your `--rm` containers.

## 10. Docs that don't match the code

- README: "leaves the other process alone" (port shadowing), "VPN DNS works" (split DNS),
  native-Mac durability, "nothing on the Mac or the local network is reachable" and "internet
  is limited to an allowlist" for sandboxes, the multi-Mac fleet example, "the bundled build"
  for the dashboard.
- `docs/agent-sandboxes.md` says `apply` warns on hook changes and checks mtime+size; it does
  neither. `docs/dashboard.md:107` says cross-origin requests are refused; cross-site GETs
  aren't. `docs/plan.md:125` promises a `strict` mount mode that doesn't exist.
- `docs/handoff.md` and `docs/phase3.md` are stale (18 tests, 28 e2e checks, repo private).
- Missing: SECURITY.md, CHANGELOG, uninstall, upgrade, troubleshooting, an index separating
  user docs from the plan/phase docs.

## Order I'd do it in

1. Section 1 (S1-S8). Small diffs each, all reachable today.
2. D1, D2, D3, pid lock and boot lock. These lose data or kill the wrong process.
3. Bump to 0.3.0 and cut a release with the dashboard bundled, plus `rekcod uninstall`.
4. Fleet reconcile: lost-node recovery, node-start VM boot, deploy lock, startup sweep, read
   timeouts. Then TLS or a hard refusal of non-tailnet binds.
5. Developer ID + notarization (needs the Apple account), version handshake, `rekcod upgrade`,
   brew tap.
6. Supervisor LaunchAgent for the VM, log rotation, `rekcod doctor`.
7. Request-level controller tests and the missing e2e cases; a self-hosted Mac runner gating
   tags.
8. Move sandbox egress enforcement to the Mac side, then get the external review the README
   already says is missing.
