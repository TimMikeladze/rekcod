# Performance plan

Where rekcod is slow today, what to try, how to tell if it worked. Ordered by expected
payoff. Each item ends with a gate: a number that decides keep or drop.

## Where we stand (2026-10-12, M5 Pro, `bench/postgres.sh`)

Matched specs (4 vCPU, ~3.8 GiB, container `--cpus 4 --memory 3g`), median of 3:

| | read-write TPS | read-only TPS | read-only from Mac TPS |
|---|---|---|---|
| rekcod | 19,981 | 148,142 | 30,423 |
| Docker Desktop | 19,082 | 152,737 | 26,127 |

Two findings drive this plan:

- From the Mac, Postgres serves 30k TPS. Inside the container it serves 150k. The
  port-forward path costs 5×, and it's what every local app talks through.
- Throughput drops as vCPUs are added: 4 vCPU 160k, 6 vCPU 130k, 8 vCPU 113k (read-only,
  uncapped). Pure compute scales fine at 8. `idle=poll`, `NO_TTWU_QUEUE`, `NO_SIS_UTIL` and
  `migration_cost_ns` don't help. Our default is 6 (the Mac's Super cores), so out of the
  box we lose about 20% on chatty workloads.

## 1. Port forwarding (biggest gap)

Today a Mac connection takes: Mac TCP → rekcodd → vsock (`FORWARD_PORT`) → agent →
`127.0.0.1:<port>` → docker-proxy → container. That's three userspace copies in the guest
and two on the Mac per message.

- **Skip docker-proxy.** The agent dials the container's IP and container port directly. It
  resolves the target itself from docker's port bindings; the forward header stays a port
  number, so nothing on the vsock can point the agent at an arbitrary address. Falls back to
  `127.0.0.1` for host-network containers. Mac-side traffic then skips the `DOCKER-USER`
  chain; document that. Rejected: `"userland-proxy": false`, because loopback DNAT needs
  `route_localnet`, which exposes guest localhost services.
- **Stop allocating per chunk in `vm::Conn`.** The pump does `buf[..n].to_vec()` into an
  unbounded channel for every read. Reuse buffers (a `BytesMut` pool), and for the forward
  path consider a bounded fast path that still always drains (the freeze rule in
  `docs/handoff.md` stays).
- **Measure each hop.** Add `bench/tcp_rtt.py`: a 64-byte echo server in a container, RTT
  p50/p99 from the Mac and from inside the guest. That tells us which hop to attack next
  (vsock itself vs our code).

### Results (2026-10-12, 6 vCPU / 8 GiB, isolated VM vs the old agent)

Done: direct dial (`crates/rekcod-agent/src/proxyport.rs`). The agent reads the target from
docker-proxy's command line, trusting only children of dockerd.

| | old (via docker-proxy) | direct |
|---|---|---|
| `bench/tcp_rtt.py` Mac p50 | 125–177 µs | 85–90 µs |
| Mac p99 | 227–419 µs | 117–133 µs |
| 8 connections | 28–32k req/s | 41–43k req/s |
| pgbench `-S` from Mac | 27.1–27.4k TPS | 35.7–36.1k TPS |

Tried and dropped: bypassing `vm::Conn` (raw vsock fd) changed nothing (88–91 µs), so the
pump task and per-chunk copies aren't the cost.

What's left is the vsock path itself. A published port reached over vmnet (Mac → guest IP)
measured 46–53 µs p50 vs 88–104 µs over vsock on the same VM. Using vmnet for forwarded
ports, with vsock as fallback, would go against ground rule 3 in `docs/handoff.md`. That's
the maintainer's call, not a default. Trade-offs and what it would take:
[`docs/vmnet-ports.md`](vmnet-ports.md).

Gate: pgbench `-S` from the Mac ≥ 45k TPS, and echo RTT p50 down by a third. Mark it a win
only if `scripts/e2e.sh` port checks still pass (UDP, IPv6 wildcard, port-in-use reporting).

## 2. vCPU count vs chatty workloads

The cost is cross-vCPU wakeups going through the hypervisor. The guest can't fix that with
scheduler knobs (tried). Options, cheapest first:

1. **Online vCPUs on demand.** Boot with the full count; the agent takes vCPUs offline
   (`/sys/devices/system/cpu/cpuN/online`) when the load is low and brings them back when
   runnable tasks exceed the online count (builds). First experiment: on a 6-vCPU VM, offline
   two and rerun pgbench. If it doesn't match the 4-vCPU numbers, drop this option.
2. **Expose cluster topology.** Check whether VZ passes a cache topology the guest can use
   with `CONFIG_SCHED_CLUSTER`, so wakeups stay on nearby vCPUs.
3. **Lower the default to 4.** Last resort; costs parallel builds. Needs a `docker build`
   comparison at 4 and 6 before deciding.

Gate: default VM matches the 4-vCPU pgbench numbers without slowing a parallel build
(e.g. compiling the guest kernel) by more than 5%.

## 3. Guest kernel trims (small, cheap)

- Off: `NUMA`, `CPU_FREQ`, `SCHED_DEBUG`. `NR_CPUS` 4096 → 64. None of these is hardening.
- Try the newer LTS (6.18). Docker Desktop runs 7.0; EEVDF and wakeup-path work landed
  after 6.12.

**Not** turning off the hardening options (`INIT_ON_ALLOC_DEFAULT_ON`,
`RANDOMIZE_KSTACK_OFFSET_DEFAULT`, `ZERO_CALL_USED_REGS`, `HARDENED_USERCOPY`, `DEBUG_LIST`,
`KFENCE`). Together they cost about 3%. The guest kernel is the only boundary between
containers running untrusted images and the shared VM, which mounts all of `/Users`.

Gate: no regression in `scripts/e2e.sh` and boot time; keep whatever is ≥ 2% on pgbench.
A kernel bump also needs the fuse patch rebased and the 3× e2e run for fuse changes.

## 4. Disk

Read-write pgbench ties Docker Desktop, but nobody has profiled the block path.

- Try `VZNVMExpressControllerDeviceConfiguration` vs virtio-blk (one queue today).
- ext4 mount options for the data disk: `noatime`, `lazytime`, and journal commit interval.
- Measure with `fio` (4k randwrite with fsync, 128k seq read) inside the guest, plus
  pgbench rw at scale 100 so the data set exceeds shared_buffers.

Gate: ≥ 10% on fio fsync randwrite with `REKCOD_DISK_SYNC` semantics unchanged.

## 5. Image pulls and builds

Unmeasured. Add a bench before choosing work: cold `docker pull` of a ~1 GB image and a
cached/uncached `docker build` of a real project, against Docker Desktop and OrbStack.
Likely levers: the vmnet NAT path for egress, containerd's snapshotter, BuildKit
parallelism. Decide after numbers.

## Benchmark suite

Every item above lands with its bench in `bench/` and its before/after in this file.
Rules for runs:

- Use an isolated VM (`REKCOD_HOME` with a short path, `REKCOD_GUEST_DIR=out/guest`), never
  the user's VM.
- Same specs on both sides; alternate engines per round; report medians of 3.
- Starting Docker Desktop switches the active docker context to `desktop-linux`; switch it
  back after.

## Order

1 (port forwarding) first: biggest gap, all host/agent code, no guest rebuild. Then the
vCPU offline experiment from 2, since it's a one-line test. 3 and 4 share a guest rebuild,
so batch them. 5 waits for its benchmark.
