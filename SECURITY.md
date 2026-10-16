# Security

## Reporting a vulnerability

Report it privately through GitHub's private vulnerability reporting:
[Security, Report a vulnerability](https://github.com/TimMikeladze/rekcod/security/advisories/new).
Please don't open a public issue or PR for it.

Include the rekcod version (`rekcod --version`), macOS version, what an attacker needs to
start with (a container, a sandbox, a network position, an API token), and the steps or a
proof of concept. I'll reply in the advisory thread, and credit you in it when the fix
ships unless you'd rather not be named.

Only the latest release gets fixes. There are no backports to older versions.

## Scope

In scope:

- The `rekcod` binary: CLI, VM daemon, docker socket proxy, port forwarder, file events,
  DNS bridge, `rekcod sandbox serve` and `rekcod mcp`.
- The guest image built by `guest/build.sh` and `rekcod-agent`.
- The fleet (`rekcod controller`, `node`, `edge`, `storage`) and its tokens.
- The web dashboard (`dashboard/`, `rekcod dashboard`).
- Release artifacts and the path to them: `release.yml`, `scripts/install.sh`, the guest
  download and its checksum.

Out of scope, report upstream instead:

- Bugs in dockerd, containerd, runc, BuildKit, the Linux kernel or Alpine packages that
  rekcod ships unmodified. Do tell me if rekcod's configuration makes one worse.
- Virtualization.framework itself (Apple).

Also out of scope: anything that needs the Mac user's account to already be compromised,
since rekcod runs with that user's permissions.

## What isn't a security boundary yet

rekcod hasn't had an external security review. These parts are defence in depth, not
isolation you should rely on against a determined attacker:

- `rekcod x --sandbox` and `rekcod sandbox` (agent sandboxes). The threat model and what
  each one doesn't protect against are in [docs/sandbox.md](docs/sandbox.md) and
  [docs/agent-sandboxes.md](docs/agent-sandboxes.md). The sandbox egress allowlist is
  enforced inside the guest, not on the Mac.
- Containers on the main VM. Any container you run can reach Mac services on `127.0.0.1`
  through `host.docker.internal`; that's a feature, and it means a hostile image can too.
- The fleet. The controller, nodes and edge are meant for a private network you control
  (a tailnet or loopback). Don't expose their admin ports to the internet.

Reports that break any of these are still welcome and get fixed. They just aren't
surprising.
