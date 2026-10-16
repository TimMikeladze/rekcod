# Sandbox network on the host

**Status:** built (`crates/rekcod/src/netstack.rs`). What changed from the plan below is
under "As built".

Moves sandbox egress filtering out of the guest. Before this, a sandbox VM got
Virtualization.framework's NAT, and the guest's own iptables (`boot.sh`, `REKCOD-ALLOW`
filled by the agent's DNS stub) decided what it could reach. Anything that got root in the
guest could flush those rules. Now the guest's network card is a socket the rekcod daemon
on the Mac reads, and the daemon decides every connection. Guest root can do whatever it
likes to its own iptables; packets still only leave through the daemon.

Scope: every sandbox VM (agent sandboxes, `rekcod x --sandbox`, fleet sandboxes, app
instances and jobs). The main VM keeps NAT; it runs your own containers and needs full
speed. macOS sandboxes are out of scope (they're plain NAT and documented as such).

## Pieces

- **Attachment.** `vm.rs` gives sandbox VMs a `VZFileHandleNetworkDeviceAttachment` on one
  end of a `socketpair(AF_UNIX, SOCK_DGRAM)`; one datagram is one Ethernet frame. MTU 1500.
  The daemon keeps the other end.
- **Stack.** `netstack.rs` runs `smoltcp` on that end: ARP, IPv4, TCP, UDP. The interface
  is the guest's gateway, `10.0.2.2/24`, with `any_ip` and a default route via itself, so
  it accepts traffic addressed anywhere. The guest is `10.0.2.15`. IPv6, ICMP and anything
  that isn't TCP or UDP is dropped.
- **Guest config.** `rekcod.net=10.0.2.15/24,10.0.2.2` on the kernel command line makes
  `boot.sh` configure `eth0` statically instead of running udhcpc. DNS keeps going over
  vsock (dnsmasq → agent stub → daemon), as now.
- **TCP.** A SYN to `dst:port` that policy allows: the daemon connects a host `TcpStream`
  to `dst:port`, opens a smoltcp socket listening on exactly that endpoint so the SYN is
  accepted, and pipes bytes both ways with backpressure from both buffers. A host connect
  that fails aborts the smoltcp socket (the guest sees a reset). A SYN policy refuses is
  answered with a reset and logged.
- **UDP.** Per guest flow (src port, dst), a host `UdpSocket` connected to `dst`; replies
  come back as frames from `dst`. Flows idle for 60 s close. Port 53 to anything is
  refused (DNS goes over vsock, where the daemon checks names).
- **Policy**, decided per connection on the Mac:
  - Never: loopback, RFC 1918, 100.64.0.0/10, link-local (169.254/16, metadata), 0/8,
    multicast, the Mac's own interface addresses, unless listed in `reach`.
  - `reach` (`IP:PORT`, app instances' RustFS): allowed.
  - Open internet (`allow` is None): any other address.
  - Allowlist: only addresses the daemon itself returned for an allowed name in the last
    DNS answers it relayed for this VM (kept per VM, with the record's TTL plus 10
    minutes). The daemon's DNS bridge checks each question against the allowlist and
    answers refused for names off it, so the guest can't learn an address any other way
    that would pass.
  - Offline: nothing (the VM gets no network card at all, as now).
- **Denials** are logged in the sandbox's `daemon.log` as
  `rekcod-egress: refused tcp 1.2.3.4:443 (not from an allowed name)`; `rekcod sandbox log
  ID --net` reads both logs.
- **The guest side stays** as a second layer: its iptables and the agent's DNS checks keep
  running, so a bug in either layer alone doesn't open the network.

## Why smoltcp and not gvisor-tap-vsock

gvisor-tap-vsock (Podman, Lima) is a Go binary with no per-connection policy hook; adding one
means a fork in another language. smoltcp is a `no_std`-friendly Rust crate already used for
exactly this userspace-NAT shape (`any_ip` plus listening on the destination endpoint).

## Cost

Sandbox traffic goes through a userspace TCP stack. I expect a few hundred Mbit/s, which is
plenty for package installs; `bench/sandbox.sh` gets a download test to check it. Port
forwarding to the Mac (autoports) doesn't use the network card and is unaffected.

## Tests

- Unit: policy decisions (private ranges, reach, allowlist with expiry), frame round trips
  through the stack with a fake guest (a second smoltcp interface on the other end of the
  socketpair) for TCP to a local listener, UDP echo, a refused SYN getting a reset.
- `scripts/e2e-sandbox.sh`: an allowed host fetches, a host off the list doesn't resolve,
  a direct connection to an IP nobody resolved is refused, the Mac's LAN address and
  10.0.0.0/8 are refused, and `iptables -F` as root in the guest changes none of that.

## As built

### Additions to the plan

- **Shared CDN addresses.** An address allowed because an allowed name resolved to it often
  serves other sites too (Cloudflare, Fastly). For allowlisted sandboxes, TCP to port 443
  is held until the guest's first bytes arrive: the TLS ClientHello's SNI must pass
  `rekcod_proto::host_allowed`, or the connection is reset and logged as
  `rekcod-egress: refused tls <sni> at <ip>`. A ClientHello without SNI is refused. Port 80
  does the same with the HTTP `Host` header (`refused http <host> at <ip>`; no Host, two of
  them or an IPv6 literal is refused). The daemon holds at most 16 KiB and waits 10 s for
  the name. Other ports get the address check only.
- **QUIC is refused** (UDP to 443) for allowlisted sandboxes. Its ClientHello sits in
  encrypted Initial packets, so the SNI check above can't see it; clients fall back to TCP.
- **One plain DNS question.** The guest agent's stub used to check only the first question
  and stop at the first compression pointer, while the whole packet went upstream. Both the
  stub (`agent.rs`) and the daemon's bridge (`dns::query_name`) now answer REFUSED for a
  query with QDCOUNT other than 1 or a compression pointer in the question. The agent
  applies it only in allowlisted sandboxes; the daemon only for sandboxes with an allowlist.
- **TCP port 53 is refused too**, not just UDP.

### Changes from the plan

- **smoltcp 0.14**, the current release, not 0.12. Features: `std`, `medium-ethernet`,
  `proto-ipv4`, `socket-tcp`, `socket-udp` (the last for the test guest). It's 0BSD, so
  `deny.toml` has exceptions for it and `managed`.
- **Connect first, then accept.** The guest's SYN is held while a host task connects to the
  real destination. On success a listener on exactly that endpoint takes the held SYN; on
  failure (refused, unreachable, 15 s timeout) smoltcp gets the SYN with no listener and
  answers with a reset. So `nc -z` to a closed port reports it closed instead of open and
  then reset. Retransmitted SYNs are dropped while the connect is pending.
- **Offline had a network card.** Before, an offline sandbox was NAT plus an empty
  allowlist. Now a sandbox with an empty allowlist and nothing in `reach` gets no network
  device; `boot.sh` logs `no network card` and skips network setup.
- **Not in the plan, decided while building:** frames whose source isn't `10.0.2.15` are
  dropped; so are IP fragments from the guest (replies to the guest are fragmented to the
  MTU when a UDP datagram is bigger). The same denial line is logged at most once per 10 s,
  so a retry loop doesn't fill `daemon.log`. The MTU is VZ's default of 1500; the
  attachment's `setMaximumTransmissionUnit` isn't called. Mac interface addresses come from
  `getifaddrs`, re-read every 10 s.
- **Logs.** The guest agent still logs `rekcod-egress: denied NAME` to the console when its
  stub refuses a name. The daemon logs `refused dns NAME`, `refused tcp|udp IP:PORT (why)`,
  `refused tls|http NAME at IP` to `daemon.log`. `rekcod sandbox log ID --net` prints the
  console lines, then the daemon's.

### Measured (2026-10-16, M5 Pro, macOS 27)

An agent sandbox with the default allowlist, busybox `wget` inside the workspace container:

| | time | rate |
|---|---|---|
| 1 GiB from a Python `http.server` on the Mac's LAN address (in `reach`), to `/dev/null` | 0.59 s, 0.62 s, 0.85 s | 1.3 to 1.8 GB/s |
| 1 GiB to `nc -l` on the Mac (in `reach`) | 2.0 s | 0.5 GB/s |
| 275 MiB Alpine ISO from dl-cdn.alpinelinux.org (`bench/sandbox.sh`) | | 29 to 45 MB/s; `curl` on the Mac got 17 to 54 MB/s in the same minutes |

The internet link is the limit for downloads. The data from the Mac matched its md5.

### Verified

`scripts/e2e-sandbox.sh` against a test home passes, including a privileged container
started through the sandbox's own docker socket that flushes the guest's `OUTPUT` and
`DOCKER-USER` chains (`nsenter -t 1 -m -u -n -i sh -c 'iptables -F; iptables -P OUTPUT
ACCEPT'`) before the network checks run again. After that flush, in an allowlisted sandbox:

```
$ rekcod sandbox exec ID -- sh -c 'wget -qO- https://registry.npmjs.org/ | head -c 100'
{}
$ rekcod sandbox exec ID -- wget -q -T 5 -O /dev/null https://example.com
wget: bad address 'example.com'
$ rekcod sandbox log ID --net     # the daemon's lines
rekcod-egress: refused tcp 1.1.1.1:443 (not from an allowed name)
rekcod-egress: refused tcp 104.16.0.35:443 (not from an allowed name)
rekcod-egress: refused tcp 192.168.12.217:22 (the Mac or local network)
rekcod-egress: refused tcp 10.0.0.1:80 (the Mac or local network)
rekcod-egress: refused tcp 10.0.2.2:22 (the Mac or local network)
rekcod-egress: refused udp 8.8.8.8:53 (DNS goes through rekcod)
rekcod-egress: refused http example.com at 104.16.1.34
rekcod-egress: refused tls 104.16.1.34 at 104.16.1.34
```

(104.16.1.34 is registry.npmjs.org's address; the last two are a `Host: example.com`
request and `wget https://104.16.1.34/`, whose SNI is the IP.) An open sandbox
(`--allow-host '*'`) reaches example.com and 1.1.1.1:443 and still can't reach the Mac's LAN
address or 169.254.169.254. An `--offline` sandbox has only `lo` and `docker0`.

Not exercised for real: a guest that replaces the agent and talks to the daemon's DNS
bridge directly. The daemon's question check has unit tests only.

### Limitations

- The SNI and Host checks read what the client claims. Domain fronting (an allowed SNI
  with another site's Host header inside TLS) gets through, and so does anything else an
  allowed host will relay.
- Server-speaks-first protocols on 80 and 443 time out after 10 s and are refused, and so
  is HTTP/2 over plain text with prior knowledge (no Host header). Encrypted Client Hello
  shows only its public name, which is refused unless it's on the list.
- `rekcod x --sandbox` gets the same network stack but still has no allowlist (open
  internet, Mac and LAN refused).
