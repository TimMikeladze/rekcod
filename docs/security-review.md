# External security review brief

What to hand a reviewer so they can test rekcod's isolation claims without reading the whole
repo first. The README says the sandboxes haven't had an external review; this is the scope
for one.

## Claims under test

1. **`rekcod x --sandbox` and `rekcod sandbox`**: code in the sandbox can't read or write
   anything on the Mac outside the project directory (for `sandbox`: a copy of it), and can't
   reach the Mac or the local network. With an allowlist, it reaches only listed hosts.
2. **`sandbox apply`**: writing a sandbox's changes back never touches a path outside the
   project and never follows a symlink.
3. **The sandbox API and MCP server** (`rekcod sandbox serve`, `rekcod mcp`): a caller gets
   only what the server was started with (`--root`, `--allow-host`, `--secret`,
   `--allow-mount`).
4. **The fleet**: a token holder can do only what its role allows, in its namespaces. A
   node's owner's files, the controller's host, other tenants' sandboxes and app instances,
   and RustFS buckets other than an app's own are out of reach. Control-plane traffic is
   TLS pinned to the controller's CA.
5. **The dashboard**: nothing but a browser holding the per-launch token can run actions;
   containers can't reach it.

## Architecture in one paragraph per boundary

- **Sandbox VM.** Its own Virtualization.framework VM and dockerd. Mac files arrive over one
  virtio-fs share (the project, or a copy under `~/.rekcod/sb/<id>/ws`). Its network card is
  a socket the rekcod daemon serves with smoltcp (`netstack.rs`); every TCP/UDP flow is a
  host socket the daemon opens after `Policy::check`, with SNI/Host checks for allowlisted
  hosts on 443/80. DNS goes over vsock to the daemon, which checks names
  (`daemon.rs::DnsFilter`). Guest iptables and the agent's DNS checks are a second layer.
- **apply.** `sandbox.rs::apply` freezes the workspace (APFS clone, container paused),
  diffs it, refuses symlinks leaving the project, then writes through `safefs.rs`: every
  component opened with `O_NOFOLLOW` from a directory fd.
- **API/MCP.** `sandbox_api.rs`: `Policy::check`/`project` gate `create`; `exec` env must be
  `KEY=VALUE`; unix socket 0600, TCP listener loopback-only with a bearer token.
- **Fleet.** `fleet.rs`: tokens hashed (SHA-256) with roles per namespace, edge tokens,
  node secrets; TLS from `tls.rs`; nodes run sandboxes for the controller only
  (`Policy::node`, node secret); the controller forwards only whitelisted fields
  (`create_body`) and authorizes the path id. Release images reach nodes as presigned URLs.
- **Dashboard.** `dashboard/src/proxy.ts`, `lib/guard.ts`, `lib/auth.ts`: token cookie,
  Host/Origin/Sec-Fetch-Site checks, argv must appear in the current `rekcod state`.

## Suggested tests

- Escape the virtio-fs share: symlinks, hardlinks, `..`, bind mounts inside the guest,
  case-insensitivity tricks (APFS) against the copy and the live mount.
- `apply` races: change `ws/` between `diff` and `apply --reviewed`; symlink swaps; very
  long paths; names that normalize differently (NFD/NFC).
- Network: raw sockets as guest root, IPv6, fragments, ARP spoofing toward the gateway,
  DNS tricks (compression, multiple questions, TCP DNS, DoH to an allowed CDN IP), SNI vs
  Host mismatches, HTTP/2 without TLS, QUIC, connections to the Mac's other interface
  addresses and to Tailscale (100.64/10).
- Kernel and VMM: what a guest kernel exploit gets (it should be the rekcod process's
  user on the Mac, and only through Virtualization.framework).
- API/MCP: path traversal in `read_file`/`write_file` (they run inside the sandbox),
  oversized requests, token timing, the TCP listener from a browser (DNS rebinding).
- Fleet: role matrix per endpoint (`fleet.rs::controller_handle`, `apps_route`,
  `jobs_route`, `namespaces_route`), IDOR on sandbox/instance/job ids, webhook forgery and
  replay, a malicious node (lying heartbeats, registering a hostile URL), `git` options and
  transports, SSRF from deploy specs, presigned URL scope.
- Dashboard: CSRF, DNS rebinding, a container or another local user reaching it.

## Setup for a reviewer

```sh
scripts/build.sh                                   # host binary and guest (needs `rekcod start` first)
scripts/e2e-sandbox.sh && scripts/e2e-fleet.sh     # what's already checked, on VMs of their own
REKCOD_HOME=~/.rkr rekcod start --no-switch        # a VM that isn't your main one
```

`docs/sandbox.md` (threat model and known limits), `docs/sandbox-network.md`,
`docs/agent-sandboxes.md`, `docs/fleet-hardening.md` and `docs/production-readiness.md`
describe the design and what's already been fixed. Known, accepted limits: domain fronting
on allowed CDNs, the shared `--sandbox` VM across projects, plain TCP between the edge and
app instances, and macOS sandboxes' unfiltered NAT.

## Reporting

Findings go through GitHub private vulnerability reporting (`SECURITY.md`). Fixes land with
a regression test in the matching e2e script or unit test, and a CHANGELOG entry under
Security.
