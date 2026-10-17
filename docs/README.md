# rekcod docs

Start with the [README](../README.md): install, what works, commands, build and test.

## Using rekcod

How a feature behaves today and how to drive it.

- [getting-started.md](getting-started.md): install to a running compose project, then snapshots, `rekcod x`, menu bar.
- [self-hosting.md](self-hosting.md): a spare Mac as your app host and docker engine, step by step.
- [lan-discovery.md](lan-discovery.md): a spare on a LAN found over Bonjour, whatever its IP or name.
- [troubleshooting.md](troubleshooting.md): what to do when something's wrong; start with `rekcod doctor`.
- [install-upgrade.md](install-upgrade.md): `rekcod upgrade`, `uninstall`, `doctor`, logs.
- [cli-passthrough.md](cli-passthrough.md): `rekcod <docker command>` without a docker CLI.
- [default-socket.md](default-socket.md): routing `/var/run/docker.sock` to rekcod.
- [sandbox.md](sandbox.md): `rekcod x --sandbox` and its threat model.
- [agent-sandboxes.md](agent-sandboxes.md): `rekcod sandbox`, the HTTP API and MCP.
- [sandbox-network.md](sandbox-network.md): sandbox egress checked by the daemon on the Mac
  (userspace TCP/IP stack, allowlist, SNI check).
- [frontends.md](frontends.md): the CLI, menu bar and dashboard share one action list.
- [dashboard.md](dashboard.md): the web dashboard and `rekcod dashboard`.
- [remote-docker.md](remote-docker.md): another Mac's rekcod as your docker engine.
- [spare.md](spare.md): provisioning a spare Mac with `rekcod host add`.
- [fleet-compute.md](fleet-compute.md): scale to zero, recycle, jobs.
- [namespaces.md](namespaces.md): multi-tenancy in a fleet.
- [edge-http.md](edge-http.md): the edge proxy, rate limits, bans, per-app HTTP settings.
- [rustfs.md](rustfs.md): app buckets, stored images and backups on RustFS.
- [fleet-hardening.md](fleet-hardening.md): fleet TLS, tokens, node approval, the reconcile loop.
- [sandbox-network.md](sandbox-network.md): sandbox egress filtered on the Mac.
- [security-review.md](security-review.md): scope and setup for an external security review.

## Building and releasing

- [guest-build.md](guest-build.md): how `guest/build.sh` builds the guest, pinned versions.
- [custom-kernel.md](custom-kernel.md): the guest kernel config and its measurements.
- [releasing.md](releasing.md): tags, release assets, notarization, the e2e runner,
  dependency checks.
- [../CONTRIBUTING.md](../CONTRIBUTING.md), [../SECURITY.md](../SECURITY.md),
  [../CHANGELOG.md](../CHANGELOG.md).

## Design and history

Specs and phase write-ups, kept for the reasoning and the numbers. Where one disagrees with
the code or a doc above, the code and the doc above win.

- [production-readiness.md](production-readiness.md): the audit the 0.3.0 work went through.
- [plan.md](plan.md): the original strategy, phases and gates.
- [mvp.md](mvp.md): phase 1, switching in one command.
- [phase2.md](phase2.md): bind-mount caching and the memory finding.
- [phase2b-4.md](phase2b-4.md): correctness gates, switcher gaps.
- [phase3.md](phase3.md): release packaging, guest auto-fetch, CI.
- [phase5.md](phase5.md): snapshots, `rekcod x`, VM-local dirs, menu bar.
- [research.md](research.md): phase 6, custom file server and libkrun (both rejected).
- [perf-plan.md](perf-plan.md): performance ideas with keep-or-drop gates.
- [vmnet-ports.md](vmnet-ports.md): forwarding ports over vmnet instead of vsock (undecided).
- [paas.md](paas.md): research on a PaaS or sandbox platform.
- [platform.md](platform.md): the platform spec (sandboxes, fleet, apps, macOS guests).
- [dashboard-sidebar.md](dashboard-sidebar.md), [dashboard-round4.md](dashboard-round4.md):
  dashboard iterations.
- [handoff.md](handoff.md): context for picking the project up in a new session.
