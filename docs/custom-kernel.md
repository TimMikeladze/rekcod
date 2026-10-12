# Custom guest kernel

Replace Alpine's stock `linux-virt` kernel with one built for rekcod's VM.

## Why

The stock kernel is generic. It probes hardware we never have, loads virtio, ext4,
netfilter and fuse as modules, and needs an initramfs just to find the root disk.
Baseline before this change (M-series, 6 vCPU, 8 GiB): kernel reaches `init` at 0.16 s,
modules loaded at 0.19–0.21 s, guest agent up 0.54–0.64 s after VM start.

## Approach

- Same version as Alpine: kernel.org sources for the version Alpine 3.22 ships
  (6.12.x LTS), so behaviour matches what we already tested. Bumping it means changing
  one `ARG`.
- Config = Alpine's `config-virt` + `guest/kernel/rekcod.config`, merged with
  `scripts/kconfig/merge_config.sh` and then `olddefconfig`. The fragment:
  - builds in (`=y`) everything boot and docker need: virtio (blk, net, console, rng,
    balloon, vsock, fs), ext4, fuse/virtiofs, overlay, bridge/veth, netfilter/nat/
    iptables, cgroups, namespaces, binfmt_misc.
  - turns off hardware VZ doesn't have: sound, DRM/GPU, wireless, USB, most NIC/SCSI/
    storage drivers, and debug options.
  - keeps modules enabled, so anything rare still loads from `/lib/modules`.
- Fuse patch goes in-tree: `guest/fuse/rekcod-fuse.patch` and `rekcod.c` apply to
  `fs/fuse` before the build. Built-in module parameters keep their
  `/sys/module/{fuse,virtiofs}/parameters/...` paths, so the agent doesn't change.
- No initramfs: virtio-blk and ext4 are built in, so the kernel mounts `/dev/vda`
  itself. The host skips `setInitialRamdiskURL` when the guest dir has no `initramfs`.
  Older guests that ship one keep working.
- Fallback: `docker build --build-arg KERNEL=alpine` still builds the stock-kernel
  guest the old way.

## Verify

On an isolated `REKCOD_HOME` (never the user's VM), stock vs custom guest:

1. Boot: kernel→init, modules loaded, and agent-up times from the console and daemon logs.
2. Memory: `MemTotal`/`MemAvailable` after boot, and image sizes.
3. Function: hello-world, stdin half-close, amd64 via Rosetta, DNS, `host.docker.internal`,
   a published port, a bind mount with cached file sharing (`rekcod_stats` present),
   `docker build`, a user bridge network, the sandbox VM.

## Results

2026-10-12, isolated test VMs (4 vCPU, 2 GiB), 5 boots each, medians:

| | stock (Alpine) | custom |
|---|---|---|
| kernel reaches `init` | 0.17 s | 0.07 s |
| `boot.sh` modules loaded | 0.21 s | 0.08 s |
| guest `ready` | 0.35 s | 0.20 s |
| agent up (daemon log) | 0.55 s | 0.39 s |
| `rekcod start` → ready | 1.0 s | 0.8 s |
| modules loaded after boot | 57 | 0 |
| `Image` + `initramfs` | 34.7 + 11.8 MB | 22.9 MB + none |
| `MemTotal` / `Slab` | 1973 / 22.3 MiB | 1975 / 18.0 MiB |

All function checks pass on both: hello-world, stdin half-close, amd64 via Rosetta,
`host.docker.internal`, external DNS, published port, Mac `/tmp` bind mount, patched
fuse (`rekcod_stats`/`rekcod_notify` present), `docker build`, user bridge network with
container DNS. The sandbox VM (`rekcod x --sandbox`) still sees only the project, reaches
the internet, and gets REJECT for the Mac gateway. `KERNEL=alpine` still builds and passes.

The memory gain is small (about 4 MiB of slab). The real win is boot: about 150 ms less
per start. A full guest build now compiles the kernel (about 8 minutes on 6 vCPUs).
