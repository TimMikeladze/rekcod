# Two edges on one fleet

Implements item 2 of `fleet-ha.md`. Adding an edge on another Mac lets an ingress
with failover keep routing when either edge Mac dies. Controller standby remains separate.

The controller already adds traffic counts from multiple edges, keeps the worst reported
percentiles, unions active hosts and adds client counts. Protect that behavior with HTTP
tests instead of replacing it. Rate limits and current-minute statistics stay per process.

`server setup --edge-only --remote fleet --join-token TOKEN` joins this Mac to an existing
controller and installs a node and edge. The configured remote supplies the controller URL,
admin token and pinned CA. The join token is required for a new node. Re-running an existing
matching node starts it again; a node joined elsewhere causes an error before service changes.
The command creates a node-specific restricted edge token, never a local controller or
RustFS. Address selection follows normal setup: explicit address, Tailscale, or the LAN.
LAN nodes advertise their `.local` hostname and listen on all interfaces.

Use the existing server setup command; `--domain` remains required for normal setup and cannot be
combined with `--edge-only`. Normal setup behavior stays the same. Power and boot service
handling apply to both modes. No SSH provisioning or external account changes in this step.

## Work

- [x] Test multi-edge traffic, clients, activity and metric aggregation through controller HTTP.
- [x] Test and implement edge-only setup command generation, validation and CLI flags.
- [x] Add a two-edge process integration test: sum traffic and clients, kill one edge during
  requests through a client that retries the second, and verify the survivor still serves.
- [x] Document the same Cloudflare tunnel on two Macs and tailnet ingress requirements;
  run the workspace suite, lint, build, integration test and independent review.

## Review focus

Do not install a controller in edge-only mode, replace another fleet's node registration,
revoke another edge's token, change primary setup's arguments, or expose the admin token in
edge service configuration. Two DNS addresses alone do not guarantee client failover.

Work inline in the current branch, preserving the previous placement changes. Validate setup
through generated commands and isolated CLI homes, without installing launchd services or
changing this Mac's power settings. Integration tests use independent controller/edge
processes and a loopback backend; edge routing does not require booting more guest VMs.

## Verification

- `cargo test --workspace --locked`: 166 host tests and 6 protocol tests passed.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`, formatting,
  release build, `git diff --check` and `packaging/server/test.sh` passed.
- `python3 scripts/e2e-edges.py`: passed with the signed release binary.
- CI-equivalent `cargo build --locked -p rekcod` followed by
  `REKCOD=target/debug/rekcod python3 scripts/e2e-edges.py`: passed. Both runs verified
  setup and repeat registration, restricted tokens, 12 + 8 reported requests and
  100 successful requests through a client with failover while an edge was terminated.
- Independent review found no important issues. No live Cloudflare or launchd
  configuration was changed. External ingress failover must be configured by the operator.

The preceding placement changes were preserved and verified in the same branch.
Controller standby remains the next HA implementation step.

CI exposed stricter certificate verification in newer Python/OpenSSL: issued server
certificates lacked their authority key identifier. Reproduced with Python 3.14, added
the extension without changing CA keys or fingerprints, and made the process test
request strict X.509 verification explicitly. Readiness failures now retain their cause.
