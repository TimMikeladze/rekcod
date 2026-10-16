# CLI and menu bar: one set of actions

rekcod has three front ends: the CLI (`rekcod ...`), the menu bar item
(`rekcod menubar`) and the web dashboard (`dashboard/`, [`dashboard.md`](dashboard.md)).
**They must always offer the same functionality.** This doc says how that's enforced.

## Rules

1. The CLI is the implementation. Every action is a CLI command line. The menu bar
   never calls docker or the daemon to change anything itself; it runs `rekcod <argv>`
   (`actions::Action`).
2. One action list. `crates/rekcod/src/actions.rs` defines `Kind` (every action),
   `Action` (a kind plus its target), and what's available for each thing shown:
   `vm_actions`, `storage_actions`, `container_actions`, `port_actions`, `project_actions`,
   `snapshot_actions`, `sandbox_actions`, `app_actions`, `job_actions`,
   `namespace_actions`, `host_actions`, `ban_actions`, `image_actions`, `volume_actions`,
   `disk_actions`, `node_actions`, `controller_actions`, `remote_actions`. The menu bar renders those
   lists as they are, so adding an action there needs no menu bar change.
3. One data model. `actions::Model::load` reads everything the menu shows (VM status,
   containers, ports, compose projects, snapshots, sandbox). `rekcod status --json` is
   built from the same status value.
4. Interactive actions (shell, logs) need a terminal: the menu bar opens Terminal.app
   with the same command line. The dashboard streams the ones that only print (logs,
   diff, `storage env`) into a panel and opens Terminal for the rest.
5. Tests: `actions::tests::every_kind_is_offered` checks every `Kind` comes out of the
   action lists; `menubar::tests::menu_offers_every_action_kind` checks the built menu
   contains every `Kind`; `tests::every_action_is_a_cli_command` (main.rs) checks each
   `argv` parses as a real CLI command (`Args::try_parse_from` or the docker passthrough);
   `state::tests::state_offers_every_action_kind` checks every `Kind` comes out of `rekcod state`.
6. The dashboard reads `rekcod state` (`state.rs`): the same `Model` with every item's
   `*_actions` attached. Its API runs only argv that appear in the current `rekcod state`.
   Its bulk bar and "Run again" add no commands: each runs actions the state already offers,
   one argv at a time, so neither needs a `Kind`.

## What each front end shows

| | CLI | Menu bar | Dashboard |
|---|---|---|---|
| VM state, CPUs, memory, disk, file cache, sandbox | `status` | top of menu | sidebar, `/vm` |
| Start / stop VM | `start` / `stop` | top of menu | sidebar VM menu |
| Compact disk | `compact` | top of menu | sidebar VM menu, `/disk` |
| Open the dashboard | `dashboard` | top of menu | – |
| Root shell | `shell` | top of menu (opens Terminal) | sidebar VM menu (opens Terminal) |
| Route /var/run/docker.sock to rekcod | `socket install` | top of menu while not installed (opens Terminal for sudo) | sidebar VM menu (opens Terminal) |
| Local S3 storage: start, show env, stop | `storage up` / `storage env` / `storage down` | top of menu while the VM runs (start and env open Terminal) | sidebar VM menu |
| Containers: start, stop, restart, remove, logs | `rekcod start/stop/restart/rm -f/logs -f <name>` | Containers › container | `/containers`, `/containers/NAME` |
| Ports: open in browser | `open http://localhost:N` | Ports | `/ports` |
| Snapshots: save, restore, delete | `snap save/restore/rm -p <project>` | Projects & snapshots › project | `/projects` |
| Agent sandboxes: shell, diff, apply, stop, delete | `sandbox shell/diff/apply/stop/rm <id>` | Agent sandboxes › sandbox | `/sandboxes`, `/sandboxes/ID` |
| macOS sandboxes: screenshot (opens in Preview) | `sandbox screenshot <id> <file> --open` | Agent sandboxes › sandbox | `/sandboxes/ID` (also an inline screenshot) |
| Apps on the default remote: open, logs, make public/private, roll back, destroy, wake (asleep) or restart instances (awake) | `app open/logs/public/private/rollback/destroy/wake/recycle <name>` | Apps › app (only with a remote) | `/apps`, `/apps/NAME` |
| Jobs on the default remote: output, cancel (running only) | `job logs/cancel <id>` | Jobs › job (only with a remote) | `/jobs`, `/jobs/ID` |
| Switch the default remote's namespace | `ns use <name>` | Namespace: current › Switch to … (only when the token is in more than one) | sidebar remote menu |
| Switch docker between this Mac's rekcod and a remote host | `host use <name>\|local` | Docker host: current › Use … (only with a host added; shown while the VM is stopped too) | sidebar docker host menu, `/fleet` |
| Forward a remote host's published ports here | `host forward <name>` | Docker host › Forward ports from … (opens Terminal) | sidebar docker host menu, `/fleet` |
| Ban an IP or range on every edge, lift a ban | `edge ban [<net>]` / `edge unban <net>` | Edge bans › Ban an IP or range (opens Terminal to ask), Unban … (only with an admin-token remote) | `/fleet` › Edge bans |
| Compose project: restart, stop, down | `compose -p NAME restart/stop/down` | Projects & snapshots › project | `/containers` (group header), `/projects` |
| Images: remove; remove dangling | `image rm ID` / `image prune -f` | Images, Free up disk space | `/images` |
| Volumes: remove (unused only); remove unused anonymous ones (never snapshots) | `volume rm NAME` / `volume prune -f --filter label!=dev.rekcod.snap` | Volumes, Free up disk space | `/volumes` |
| Clear the build cache | `builder prune -f` | Free up disk space | `/disk` |
| Run a finished job again | `job rerun ID` | Jobs › job | `/jobs`, `/jobs/ID` |
| Drain a node, undrain it (admin token) | `node drain/undrain ID` | Nodes › node | `/fleet`, `/nodes/ID` |
| Back up the controller (admin token) | `controller backup` | Remote › … | sidebar remote menu, `/fleet` |
| Make another remote the default | `remote use NAME` | Remote › Use remote … | sidebar remote menu, `/fleet` |
| Ban one of the busiest client IPs (admin token) | `edge ban IP` | Edge bans › Ban IP (N requests) | `/fleet` › Edge bans |

Creating, exec, fork, `cp` and golden disks are for agents and scripts, so they're CLI,
HTTP and MCP only. So are deploys, scaling and env changes (they take arguments; the dashboard's app page has
"Copy command" with the line filled in), `node pool` (takes namespaces; the node page shows the command), the
other fleet admin commands (`node join/serve/pool`, `controller serve/token/restore`, `edge serve`, `remote add/rm`, `spare`), `host add/rm`, `--remote` sandboxes,
`sandbox input` (agents), and `macos` image setup (one-time, interactive). The menu bar lists what exists and offers what a person does to it.

## Changing things

When you add a CLI feature a user would act on, add a `Kind` and put it in the right
`*_actions` list. Update this table (all three columns) and the README. The tests fail until every kind is
reachable.
