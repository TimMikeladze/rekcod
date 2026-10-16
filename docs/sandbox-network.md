# Sandbox network on the host

Moves sandbox egress filtering out of the guest. Today a sandbox VM gets Virtualization.
framework's NAT, and the guest's own iptables (`boot.sh`, `REKCOD-ALLOW` filled by the
agent's DNS stub) decide what it may reach. Anything that gets root in the guest can flush
those rules. After this, the guest's network card is a socket the rekcod daemon on the Mac
reads, and the daemon decides every connection. Guest root can do whatever it likes to its
own iptables; packets still only leave through the daemon.

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
