# Fleet placement and recovery

Implements item 1 of `fleet-ha.md`. A lost worker must not cause healthy replicas to
restart or change the live release number. This does not add controller failover.

Use the existing controller and SQLite store. Rank ready nodes by app replicas in
their zone, then on their node, then free memory. Reserve memory for every assignment.
An unlabelled node is its own failure domain; `spread = "strict"` requires distinct
nodes and distinct labelled zones. Default spreading is best effort and reports sharing.

The deploy and recovery paths share launch, health-check and registration code. Failed
nodes are excluded for the current attempt. Deploys still switch only after all requested
replicas are healthy; recovery keeps every successful replacement if capacity runs out.
Recovery uses the existing release and retires only the lost replicas it replaces.
Draining keeps existing routes until replacements are healthy, and retries incomplete
moves through reconciliation. Sleeping apps stay asleep during ordinary reconciliation.

No new service or dependencies. `node join --zone` and `rekcod.toml`'s
`resources.spread` extend existing actions; status exposes placement and sharing.

## Work

- [x] Add tests for capacity accounting, spreading, zones, strict mode and pools;
  implement placement and configuration.
- [x] Add HTTP-boundary tests for recovery and failed-node retries; share instance
  launch code and replace full redeployment with partial top-up.
- [x] Test safe drain and returning lost nodes; update fleet e2e assertions and docs.
- [x] Run `cargo test --workspace`, formatting and build checks; review the diff.

## Verification

Use in-memory SQLite and loopback HTTP node fixtures for controller decisions. Real
VM checks use isolated homes only, never the user's VM. Existing fleet baseline:
`cargo test -p rekcod fleet::tests -- --test-threads=4`: 15 passed.

## Decisions

Work in the current checkout on `feat/fleet-placement-recovery` so the changes remain
visible in the shared workspace. Implement inline. The existing HA design and the user's
instruction to start its first step supply the scope; no additional approval handoff.

## Checks so far

- `cargo test --workspace --locked`: 163 host tests and 6 protocol tests passed.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- Release build, formatting, shell syntax and `packaging/server/test.sh`: passed.
- Independent review found health-retry route resurrection and strict-zone reclaim;
  both have failing-then-passing regression tests and fixes. A drain/deploy race also
  has a regression test and fix.
- First VM run stopped because the local build lacked the virtualization entitlement.
  Signed a copy with `rekcod.entitlements`; the rerun uses fresh isolated homes.
- Final review also covered malformed node replies: a regression test first failed
  with `node returned no endpoint`; per-node transactional registration now preserves
  successful partial recovery and cleans unregistered sandboxes. Review found no
  remaining important issues.

## Final result

`cargo test --workspace --locked`: 169 tests passed. Clippy with warnings denied,
formatting, shell syntax, release build and server packaging checks passed.
`scripts/e2e-fleet.sh`: all 94 checks passed on the final signed binary, including
three-Mac spreading and recovery to three replicas after a worker was killed. The
release number and both surviving sandbox IDs stayed unchanged.

The integration harness now uses fresh temporary homes, an explicit controller TLS
name, and checks sleeping VMs on all three nodes. Its first post-deploy request allows
the ordinary wake hold because the 5-second test idle timer can expire during a cold
build. Earlier interrupted/failed runs were replaced by the complete passing run.

Changes were verified on `feat/fleet-placement-recovery` before integration. Controller standby and
edge/storage failover are outside this implementation and remain planned.
