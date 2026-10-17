# Rosetta and amd64 images

rekcod runs `linux/amd64` images through Rosetta for Linux, which only works in a
Virtualization.framework (VZ) VM on Apple Silicon. That is most of the reason rekcod is
Apple Silicon only.

```sh
docker run --rm --platform linux/amd64 alpine uname -m   # x86_64
```

## How it works

- At boot the daemon attaches Rosetta to the VM as a virtio-fs share tagged `rosetta`
  (`crates/rekcod/src/vm.rs`), but only if `VZLinuxRosettaDirectoryShare::availability()`
  says Rosetta is installed on the Mac.
- `guest/rootfs/etc/rekcod/boot.sh` mounts it at `/mnt/rosetta` and registers
  `/mnt/rosetta/rosetta` with `binfmt_misc` for x86-64 ELF binaries. The `F` flag opens the
  interpreter at registration, so it works inside every container without a mount.
- `daemon.log` records which one you got: `VM started (6 CPUs, 8192 MiB, rosetta=true)`.

## When Rosetta isn't installed

A Mac that has never run an Intel app may not have Rosetta. The VM then boots without the
share, and every amd64 container fails with `exec format error`. rekcod doesn't install
Rosetta or warn about it; the only sign is `rosetta=false` in `~/.rekcod/logs/daemon.log`.
Install it, then restart the VM:

```sh
softwareupdate --install-rosetta --agree-to-license
rekcod stop && rekcod start
```

## Why VZ instead of another hypervisor

- Rosetta for Linux guests exists only under VZ. libkrun, Docker's own VMM and QEMU all
  run amd64 under QEMU emulation, which is several times slower.
- VZ also brings Apple-maintained virtio-fs, networking and memory balloon, so rekcod
  doesn't maintain a VMM.
- The `Vmm` trait keeps a libkrun backend possible (`plan.md`, `research.md`). It would
  still be Apple Silicon only and would lose Rosetta.

## Rosetta after macOS 27

Apple says macOS 27 is the last release with general-purpose Rosetta for Mac apps. What I
found says Rosetta in Linux VMs carries on, but it came from blogs and forums, not
Apple's own release notes, so treat it as unconfirmed. On this Mac (macOS 27.0.1, macOS 27
SDK) nothing is deprecated: the `VZLinuxRosetta*` classes carry no deprecation marks, and
`/Library/Apple/usr/libexec/oah/RosettaLinux` holds `rosetta` and `rosettad`. If Apple
does drop it, amd64 images fall back to QEMU speed or stop working, and the case for VZ
over libkrun gets much weaker.

## Translation caching (tried, not shipped)

VZ can point Rosetta at a caching daemon (`VZLinuxRosettaUnixSocketCachingOptions`, macOS
14+). `rosettad daemon <dir>` in the guest stores ahead-of-time translations in `<dir>`
and listens at `<dir>/uds/rosetta.sock`. The VZ default socket path is
`/run/rosettad/rosetta.sock`, so the test passed `/var/lib/docker/rosettad/uds/rosetta.sock`
(the data disk) with `initWithPath`. The daemon has to run outside any container's cgroup,
or Docker kills it along with that container.

Measured on 2026-10-16 in a throwaway VM (own `REKCOD_HOME`), five `node -e
'require("http")'` runs per container, `node:22-slim` amd64:

| Setup | Per start |
|---|---|
| No cache (rekcod today) | 179 to 208 ms |
| Cache on, plain `docker run` | 176 to 208 ms |
| Cache on, socket bind-mounted into the container | first ever 2447 ms, then 42 to 66 ms |

Plain containers get nothing because Rosetta runs inside the container and connects to
the socket path in the container's own mount namespace, where it doesn't exist. It then
falls back to translating without a cache, with no error. The abstract-socket option has
the same problem one layer down: abstract sockets live in the network namespace, and
containers get their own unless they use `--network host`. Docker has no setting that
adds a mount to every container, so shipping this would mean wrapping the OCI runtime to
inject the mount into each container's config.

Why it isn't in:

- It only helps processes that start over and over from the same amd64 image: test runs,
  builds running x86 compilers, CLIs packaged as containers. Long-running services start
  once and gain nothing. arm64 images don't use Rosetta at all.
- The first start of each new binary gets slower (2.4 s against 0.2 s above), which hurts
  one-off and freshly tagged images.
- The cache grows with no limit: node alone took 59 MB.
- Every container would go through a runtime wrapper rekcod owns.

Worth revisiting if a lot of users turn out to run amd64-only images (internal images
built only for x86 production, old pins like `mysql:5.7`, vendor images).
