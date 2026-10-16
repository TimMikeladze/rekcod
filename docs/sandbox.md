# `rekcod x --sandbox`: threat model

**Status:** built and tested by the author (`scripts/e2e.sh`, the manual checks below).
No external security review yet, so treat it as defence in depth, not a security boundary
to market.

## What it's for

You want to run something you don't trust with your project:
- `npm install` of a package whose install script might be a worm. Shai-Hulud (Sept 2025)
  harvested npm, GitHub and cloud tokens from developer machines.
- An AI agent's shell commands.

The command should be able to read and write the project and download from the internet.
It shouldn't be able to read the rest of your machine or reach anything else on it.

## How it's built

`rekcod x --sandbox` boots a **second VM**, separate from the main rekcod VM:

| | Main VM | Sandbox VM |
|---|---|---|
| Mac directories shared | `/Users`, `/Volumes`, `/private` | only the current directory, at its real path |
| State | `~/.rekcod/vm` | `~/.rekcod/sandbox/vm` (own data disk, MAC address, sockets) |
| `host.docker.internal` → Mac loopback | yes (vsock splice) | **no**: the daemon doesn't park reverse connections |
| Network card | VZ NAT | a socket the rekcod daemon on the Mac serves; every connection is checked there (docs/sandbox-network.md) |
| Mac / LAN addresses (the gateway, 10/8, 172.16/12, 192.168/16, 169.254/16, 100.64/10, loopback, the Mac's own addresses) | reachable | **refused** by the daemon, which answers the SYN with a reset. iptables on `eth0` (`OUTPUT` and `DOCKER-USER`, set up before dockerd starts) reject the same ranges as a second layer. No IPv6 |
| DNS | Mac resolvers over vsock | same (needed for the internet) |
| Environment | `.env`, `-e` | only `-e` and `--secret NAME`; `.env` is **not** passed |
| Lifetime | until `rekcod stop` | powered off when the command exits (`--keep` to leave it up) |

Why a separate VM rather than a container in the main VM: the main VM mounts all of
`/Users`, so a container escape there (a kernel bug, a misconfigured mount) reaches
all of `$HOME`. In the sandbox VM, a full guest-root compromise still only sees the
project directory, and can't reach the Mac or the LAN: flushing the guest's iptables
changes nothing, because the network checks run in the daemon on the Mac.

## What it protects against

- Reading secrets outside the project. `~/.ssh`, `~/.aws`, `~/.npmrc`, browser profiles
  and other repos don't exist in the sandbox VM, not even as empty directories (only the
  parents of the project path are created).
- Reaching Mac services. Databases, dev servers and admin UIs on the Mac (loopback
  or LAN-bound) aren't reachable. Neither are other machines on the LAN or link-local
  metadata addresses.
- Persisting outside the project. Writes land in the project directory (visible on the
  Mac, as intended) or in the sandbox VM's disk, which never touches the main VM.

## What it does not protect against

- The project itself. The command can read and change everything in the current
  directory and any secrets you keep in it. Run it from a directory that holds only what
  the command needs. `.git` is mounted read-only, so it can't plant a hook or
  `core.fsmonitor` that your next `git status` would run on the Mac.
- Exfiltration over the internet. The internet is open (installs need it). There is
  no egress allowlist yet; `--offline` removes the network entirely.
- Secrets you pass in. `--secret NAME` hands the value to the command.
- Virtualization.framework escapes. A VM escape lands in the rekcod process on the
  Mac, with your user's permissions.
- Published ports. Ports the command listens on are forwarded to the Mac's
  `localhost`, like any `rekcod x`. A malicious server could serve hostile content to your
  browser there. `--offline` doesn't change this; `--no-autoports` without `-p` does.
- Images. The sandbox pulls the image itself; a malicious image is still malicious,
  just contained.
- Timing. One sandbox VM runs at a time. Starting one for another directory stops the
  previous one.

## Verified (2026-10-12, M5 Pro, macOS 27)

From inside `rekcod x --sandbox --image alpine:3.22` in `~/sbtest`, with a Python HTTP
server on the Mac at `0.0.0.0:18099`:

- the project file is readable, and a write appears on the Mac;
- `ls /Users` shows only the path down to the project; `~/.ssh` doesn't exist;
- `host.docker.internal:18099`, the default gateway, and `192.168.64.1:18099` are all refused;
- `https://example.com` loads;
- the same holds with `--no-autoports` (bridge network, `DOCKER-USER` rules);
- the VM powers off after the command (`rekcod status` shows no sandbox).

`scripts/e2e.sh` checks the directory isolation and the Mac's unreachability on every run.

2026-10-16, after the network moved to the daemon (docs/sandbox-network.md): `rekcod x
--sandbox --image alpine:3.22` gets `eth0` at 10.0.2.15, loads `https://example.com`, and
the Mac's LAN address on port 22 is refused. The checks with the guest's firewall flushed
ran in agent sandboxes (`scripts/e2e-sandbox.sh`), which use the same code path.

## Before calling it a security feature

1. External review of the share setup (`daemon.rs` sandbox options, `boot.sh` project
   mount and firewall rules).
2. An egress allowlist (`--allow-host`), enforced on the Mac. Done for `rekcod sandbox`
   (docs/agent-sandboxes.md, docs/sandbox-network.md), not yet for `rekcod x --sandbox`.
3. A per-run disposable data disk, so one sandboxed run can't plant images for the next.
   Done for `rekcod sandbox` (each one clones a golden disk and deletes it on `rm`).
