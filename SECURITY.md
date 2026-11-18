# Security

## Reporting a vulnerability

Report it privately through GitHub's private vulnerability reporting:
[Security, Report a vulnerability](https://github.com/TimMikeladze/rekcod/security/advisories/new).
Please don't open a public issue or PR for it.

Include the rekcod version (`rekcod --version`), macOS version, what an attacker needs to
start with (a container, a sandbox, a network position, a token), and the steps or a proof
of concept. I'll reply in the advisory thread, and credit you in it when the fix ships
unless you'd rather not be named.

## Supported versions

Only the latest release gets fixes. There are no backports to older versions.

## Scope

In scope:

- The `rekcod` binary: CLI, VM daemon, docker socket proxy, port forwarder, file events,
  DNS bridge, `rekcod sandbox serve` and `rekcod mcp`.
- The guest image built by `guest/build.sh` and `rekcod-agent`.
- The fleet (`rekcod controller`, `node`, `edge`, `storage`) and its tokens.
- The web dashboard (`dashboard/`, `rekcod dashboard`).
- Host operations: `host add`, `server setup`, the server pkg, `upgrade` and `uninstall`.
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
isolation to rely on against a determined attacker. Reports that break them are still
welcome and get fixed.

- `rekcod x --sandbox`. Its internet is open, ports it listens on reach the Mac's loopback,
  the command can change any file in the directory except `.git`, and one VM with one image
  store and shared caches serves every project
  ([README, "Run it in a separate VM"](README.md#run-it-in-a-separate-vm)).
- Agent sandboxes (`rekcod sandbox`). The egress allowlist and the block on the Mac and LAN
  are enforced by the daemon on the Mac: a sandbox's network card is a socket the daemon
  serves, so root in the guest flushing its own iptables changes nothing. The guest's
  iptables and DNS checks stay as a second layer. The SNI and `Host` checks trust what the
  client sends, so domain fronting through an allowed CDN passes, and the default allowlist
  includes hosts that accept uploads. macOS sandboxes are plain NAT with no allowlist; the
  Mac and LAN are reachable
  ([README, "Sandbox limitations"](README.md#sandbox-limitations)).
- Containers on the main VM. Any container can reach Mac services on `127.0.0.1` through
  `host.docker.internal`; that's a feature, and it means a hostile image can too. Only the
  dashboard's port is refused. A plain `-p` publishes on every interface of the Mac.
- The fleet. The controller, nodes, edge and RustFS are meant for a private network you
  control (a tailnet or loopback); don't expose their ports to the internet. Edge to
  instance traffic is plain HTTP
  ([README, "What the fleet trusts"](README.md#what-the-fleet-trusts)).
- The dashboard token keeps out other machines and containers, not other processes running
  as you.

## Releases

`install.sh` and `rekcod upgrade` check the tarball against the release's `SHA256SUMS`.
That file comes from the same release, so on its own it only catches a damaged download.
With `gh` installed they also run `gh attestation verify` against the build provenance
`release.yml` publishes, using the release's `rekcod.sigstore.json` asset and falling back to
GitHub's API. Over a Developer ID-signed rekcod, the new binary must be signed by the same
team. Without `gh`, only the checksum is checked, and both say so
([README, "What upgrade verifies"](README.md#what-upgrade-verifies)).

## Host operations

- A Bonjour answer is never used as an `ssh` argument unchecked: it has to be a plain host
  name, and a LAN server's SSH host key is pinned at `host add`, so `host refresh` and the
  others follow a new name only to a Mac that holds that key.
- `server setup` passes the admin, edge and join tokens to the services through their
  environment, never their arguments. `controller serve --alert-webhook --install` keeps the
  URL in a 0600 file, not in the world-readable plist. Plists never get `AWS_ACCESS_KEY_ID`,
  `AWS_SECRET_ACCESS_KEY` or `GITHUB_TOKEN`.
- Credentials in git URLs are redacted in node logs, in `GET /apps` for viewers and in
  `doctor` bundles.
- Commands rekcod runs as root (`sudo`, `pmset`, `plutil`, `defaults`, `fdesetup`,
  `shutdown`) are called by absolute path, and `uninstall` replays only `pmset` lines from
  `pmset-before.json`, as `/usr/bin/pmset`.
- `host add` creates an unencrypted SSH key at `~/.rekcod/ssh/id_ed25519` and trusts the
  other Mac's host key on first connect
  ([README, "What `host add` changes"](README.md#what-host-add-changes)).
