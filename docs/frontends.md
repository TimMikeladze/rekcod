# CLI and menu bar: one set of actions

rekcod has two front ends: the CLI (`rekcod ...`) and the menu bar item
(`rekcod menubar`). **They must always offer the same functionality.** This doc says
how that's enforced.

## Rules

1. The CLI is the implementation. Every action is a CLI command line. The menu bar
   never calls docker or the daemon to change anything itself; it runs `rekcod <argv>`
   (`actions::Action`).
2. One action list. `crates/rekcod/src/actions.rs` defines `Kind` (every action),
   `Action` (a kind plus its target), and what's available for each thing shown:
   `vm_actions`, `container_actions`, `port_actions`, `project_actions`,
   `snapshot_actions`. The menu bar renders those lists as they are, so adding an
   action there needs no menu bar change.
3. One data model. `actions::Model::load` reads everything the menu shows (VM status,
   containers, ports, compose projects, snapshots, sandbox). `rekcod status --json` is
   built from the same status value.
4. Interactive actions (shell, logs) need a terminal: the menu bar opens Terminal.app
   with the same command line.
5. Tests: `actions::tests::every_kind_is_offered` checks every `Kind` comes out of the
   action lists; `menubar::tests::menu_offers_every_action_kind` checks the built menu
   contains every `Kind`; `tests::every_action_is_a_cli_command` (main.rs) checks each
   `argv` parses as a real CLI command (`Args::try_parse_from` or the docker passthrough).

## What each front end shows

| | CLI | Menu bar |
|---|---|---|
| VM state, CPUs, memory, disk, file cache, sandbox | `status` | top of menu |
| Start / stop VM | `start` / `stop` | top of menu |
| Compact disk | `compact` | top of menu |
| Root shell | `shell` | top of menu (opens Terminal) |
| Route /var/run/docker.sock to rekcod | `socket install` | top of menu while not installed (opens Terminal for sudo) |
| Containers: start, stop, restart, remove, logs | `rekcod start/stop/restart/rm -f/logs -f <name>` | Containers › container |
| Ports: open in browser | `open http://localhost:N` | Ports |
| Snapshots: save, restore, delete | `snap save/restore/rm -p <project>` | Snapshots › project |
| Agent sandboxes: shell, diff, apply, stop, delete | `sandbox shell/diff/apply/stop/rm <id>` | Agent sandboxes › sandbox |
| Apps on the default remote: open, logs, roll back, destroy | `app open/logs/rollback/destroy <name>` | Apps › app (only with a remote) |

Creating, exec, fork, `cp` and golden disks are for agents and scripts, so they're CLI,
HTTP and MCP only. So are deploys, scaling and env changes (they take arguments), the
fleet's admin commands (`node`, `controller`, `edge`, `remote`), and `--remote` sandboxes. The menu bar lists what exists and offers what a person does to it.

## Changing things

When you add a CLI feature a user would act on, add a `Kind` and put it in the right
`*_actions` list. Update this table and the README. The tests fail until every kind is
reachable.
