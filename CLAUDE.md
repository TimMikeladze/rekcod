# rekcod

## CLI and menu bar stay in sync

The CLI and the menu bar (`rekcod menubar`) must always offer the same functionality.

- Every user-facing action is a CLI command first.
- The menu bar only renders the lists in `crates/rekcod/src/actions.rs`
  (`vm_actions`, `storage_actions`, `container_actions`, `port_actions`, `project_actions`,
  `snapshot_actions`, `sandbox_actions`, `app_actions`, `job_actions`, `namespace_actions`, `host_actions`, `ban_actions`,
  `image_actions`, `volume_actions`, `network_actions`, `engine_actions`, `disk_actions`, `node_actions`, `controller_actions`,
  `remote_actions`) and runs the action's `rekcod` command line. Another Mac's engine comes from
  `rekcod host state`, whose actions run through `rekcod host exec` (docs/engines-and-objects.md).
- When adding a CLI feature a user acts on, add a `Kind` and put it in the right list
  in the same change, then update the README and `dashboard/fixtures/state.json`.
- An action that needs a value declares an `Input` and puts `{key}` in its argv. Validation
  lives in `actions::invalid` and `dashboard/src/lib/inputs.ts`; change both together.
- The web dashboard (`dashboard/`) renders the same lists from
  `rekcod state` (`state.rs`) and runs only the argv found there: as is, with an action's
  `{key}` slots filled after `lib/inputs.ts` checks the values, or from a selected host's
  `rekcod host state` (each one a `host exec`).
- The parity tests (`actions::tests::every_kind_is_offered`,
  `menubar::tests::menu_offers_every_action_kind`, `tests::every_action_is_a_cli_command`,
  `state::tests::state_offers_every_action_kind`) must pass.

## Testing against the real VM

The rekcod VM on this machine runs the user's own containers. Before `rekcod stop`, a VM
restart, `scripts/e2e.sh`, or any test that clicks actions on "the first row", run
`docker ps`. Act only on containers you created; ask before restarting the VM while the
user's containers run (`--rm` ones are deleted when it stops).
