# Forwarded ports over vmnet

Should published ports reach containers over the vmnet network (Mac → guest IP) instead of
vsock? Not decided. This is what we know.

## Where we are

Mac TCP to a published port goes Mac → rekcodd → vsock → agent → container. Since the agent
started dialing the container directly instead of docker-proxy (`docs/perf-plan.md` §1),
measured with `bench/tcp_rtt.py` (64-byte echo, M5 Pro, 6 vCPU / 8 GiB):

| path | p50 round trip | 8 connections |
|---|---|---|
| old agent (vsock + docker-proxy) | 125–177 µs | 28–32k req/s |
| vsock, direct to container (today) | 85–90 µs | 41–43k req/s |
| vmnet, Mac → guest IP | 46–53 µs | not measured |
| Docker Desktop, same bench | 143 µs | 27–30k req/s |
| inside the container (loopback floor) | 20 µs | |

pgbench `-S` from the Mac is 36k TPS today. I'd guess vmnet gets it near 50k; that's not
measured.

Bypassing `vm::Conn` (raw vsock fd) changed nothing, so what's left over vsock is
Virtualization.framework's vsock path itself, not our code.

## What skipping it costs

About 35 µs per request. One request won't show it. A page making 50 sequential queries
loses about 1.75 ms. Throughput-bound work (load tests, chatty ORMs) stays around 36k TPS
instead of an estimated 50k. Bulk transfer wasn't measured.

Without it rekcod is still faster than Docker Desktop on this path. OrbStack wasn't measured;
run `python3 bench/tcp_rtt.py --context orbstack` to see.

## If we do it

Ground rule 3 in `docs/handoff.md` is "vsock only, no reliance on vmnet routing". A fast path
that falls back to vsock keeps the no-reliance part: vsock stays the guaranteed path. The rule
would read "vsock is the guaranteed path; vmnet is an optional fast path".

It **must** have:

1. A source filter. The agent only accepts forwarded traffic on `eth0` from the Mac's vmnet
   gateway address. Other VMs on the vmnet bridge (UTM, rekcod's sandbox VM) must not reach
   container ports. Without this, don't ship it.
2. Ports published on `127.0.0.1`. Their docker-proxy listens on guest loopback, so vmnet
   can't reach it. The agent DNATs the gateway source to the container address it already
   resolves (`proxyport.rs`).
3. A probe and fallback. Try vmnet per port with a ~50 ms connect timeout, cache the result,
   fall back to vsock and retry later. A hung connect must never stall the client.
4. e2e checks for the vmnet path, the forced fallback (route blocked), and that the sandbox
   VM can't reach the main VM's ports.

On by default, `REKCOD_PORTS_VSOCK_ONLY=1` to turn it off.

Open risks:

- VPN clients that accept the connect but black-hole the data. The connect timeout doesn't
  catch that.
- Mac firewalls (Little Snitch, LuLu) may prompt on the first connection.
- `192.168.64.0/24` colliding with a VPN or LAN route.

## Recommendation

Worth doing for chatty workloads, but optional. If the extra network surface and VPN edge
cases aren't worth 35 µs a request, skipping it costs little.
