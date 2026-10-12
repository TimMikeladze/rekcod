# rekcod

## CLI and menu bar stay in sync

The CLI and the menu bar (`rekcod menubar`) must always offer the same functionality.
See `docs/frontends.md`.

- Every user-facing action is a CLI command first.
- The menu bar only renders the lists in `crates/rekcod/src/actions.rs`
  (`vm_actions`, `container_actions`, `port_actions`, `project_actions`,
  `snapshot_actions`) and runs the action's `rekcod` command line.
- When adding a CLI feature a user acts on, add a `Kind` and put it in the right list
  in the same change, then update `docs/frontends.md` and the README.
- The parity tests (`actions::tests::every_kind_is_offered`,
  `menubar::tests::menu_offers_every_action_kind`, `tests::every_action_is_a_cli_command`)
  must pass.

## Testing against the real VM

The rekcod VM on this machine runs the user's own containers. Before `rekcod stop`, a VM
restart, `scripts/e2e.sh`, or any test that clicks actions on "the first row", run
`docker ps`. Act only on containers you created; ask before restarting the VM while the
user's containers run (`--rm` ones are deleted when it stops).
