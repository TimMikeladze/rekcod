# CLI, TUI and menu bar: one set of actions

rekcod has three front ends: the CLI (`rekcod ...`), the TUI (`rekcod tui`, or bare
`rekcod` in a terminal) and the menu bar item (`rekcod menubar`). **They must always
offer the same functionality.** This doc says how that's enforced.

## Rules

1. **The CLI is the implementation.** Every action is a CLI command line. The TUI and
   the menu bar never call docker or the daemon to change anything themselves; they run
   `rekcod <argv>` (`actions::Action::argv`).
2. **One action list.** `crates/rekcod/src/actions.rs` defines `Kind` (every action),
   `Action` (a kind plus its target), and what's available for each thing on screen:
   `vm_actions`, `container_actions`, `port_actions`, `project_actions`,
   `snapshot_actions`. Both UIs render those lists as they are. Adding an action means
   adding it there; both UIs pick it up with no UI change.
3. **One data model.** `actions::Model::load` reads everything the UIs show (VM status,
   containers, ports, compose projects, snapshots, sandbox). `rekcod status --json` is
   built from the same status value.
4. **Interactive actions** (shell, logs) need a terminal: the TUI suspends itself and runs
   them in place; the menu bar opens Terminal.app with the same command line.
5. **Tests:** `actions::tests::every_kind_reaches_both_uis` checks that every `Kind` comes
   out of the per-thing action lists; `menubar::tests::menu_offers_every_action_kind`
   checks the built menu contains every `Kind`; the TUI tests render the screen and run
   actions by key; `tests::every_action_is_a_cli_command` (main.rs) checks each `argv`
   parses as a real CLI command (`Args::try_parse_from` or the docker passthrough route).

## What each front end shows

| | CLI | TUI | Menu bar |
|---|---|---|---|
| VM state, CPUs, memory, disk, file cache, sandbox | `status` | header | top of menu |
| Start / stop VM | `start` / `stop` | `v` menu | Start / Stop (top of menu) |
| Compact disk | `compact` | `v` menu, `c` | top of menu |
| Root shell | `shell` | `v` menu, `!` (suspends the TUI) | top of menu (opens Terminal) |
| Containers: start, stop, restart, remove, logs | `rekcod start/stop/restart/rm -f/logs -f <name>` | Containers tab: `s` `x` `r` `d` `l` | Containers › container |
| Ports: open in browser | `open http://localhost:N` | Ports tab | Ports |
| Snapshots: save, restore, delete | `snap save/restore/rm -p <project>` | Snapshots tab: `n` `r` `d` | Snapshots › project |

## Changing things

When you add a CLI feature a user would act on, add a `Kind` and put it in the right
`*_actions` list. Update this table and the README. The test fails until every kind is
reachable.
