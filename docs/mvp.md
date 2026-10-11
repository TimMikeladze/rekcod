# rekcod MVP: seamless switch

Goal: someone on Docker Desktop or OrbStack switches to rekcod in one command,
keeps their images, volumes and compose files, and can switch back in one command.
Adoption comes before speed work (see `plan.md` §4 phase 1).

```
rekcod start            # boot the VM, create and select the `rekcod` docker context
rekcod import --from desktop-linux   # or orbstack: images + volumes
docker compose up -d    # unchanged
rekcod stop             # power off, restore the previous docker context
```

## Decisions

| Area | Choice | Why |
|---|---|---|
| Engine | Upstream static `dockerd` 29.9.0 (download.docker.com), unmodified | 100% API parity, embedded BuildKit, pinned version |
| VMM | Virtualization.framework via `objc2-virtualization` | Rosetta, Apple-maintained virtio-fs, net and balloon |
| Guest OS | Alpine 3.22 `linux-virt` 6.12 kernel (4K pages) + initramfs, ext4 root image, busybox init | The stock kernel has virtio-pci built in and the rest as modules; no custom kernel yet |
| Host↔guest | vsock only: 2375 docker API, 2376 port forward, 2377 control, 2378 DNS | No dependence on vmnet routing (VPN-safe), no TCP API exposed |
| Files | One virtio-fs device sharing `/Users`, `/Volumes` and `/private`, mounted at the **same paths** in the guest (plus `/var/folders`) | Bind-mount paths need no translation |
| Ports | Host daemon watches `/events` and mirrors each published port on the Mac (`0.0.0.0` maps to `[::]` dual-stack, `127.0.0.1` to loopback), then tunnels it over vsock to guest `127.0.0.1:<HostPort>` | `localhost:3000` works like Docker Desktop |
| A Mac port is taken | Log it, show it in `rekcod status`, never kill anything | User rule |
| DNS | Guest dnsmasq answers `host.docker.internal` and `gateway.docker.internal` (the vmnet gateway). Everything else goes to the agent's stub, then over vsock to the daemon, which queries the Mac's `/etc/resolv.conf` servers | The vmnet gateway refused DNS on the test Mac, and the Mac's resolver was link-local IPv6, which the VM can't reach |
| File events | The daemon watches bind sources of running containers with FSEvents and forwards a path when its (mtime, size, inode) changes. The agent revalidates the dentry (a `link()` from a tmpfs anchor forces an exclusive-create lookup and always fails with EEXIST or EXDEV), then does a same-mode `chmod`, which raises IN_ATTRIB | Without this, hot reload never fires, and atomic saves leave a stale dentry (ENOENT) |
| x86 | Rosetta share plus a binfmt_misc `CF` registration | `--platform linux/amd64` at Rosetta speed |
| Disks | `root.img` cloned from the release (APFS clone), `data.img` 64 GiB sparse ext4 at `/var/lib/docker` | Upgrades replace root; user state lives on data |
| Context | `docker context create rekcod` + `use`; the previous context is saved and restored on `stop` | The user changes nothing; switching back is one command |
| Import | Images: `docker save \| docker load` per image. Volumes: tar stream between helper containers, with names and labels preserved | Compose recreates containers; images and volumes are the state |
| Clock | Host pushes its time over control every 30 s | Guest clock drifts after Mac sleep |
| Boot | `virtio-rng` loads from the initramfs (`modules=`) | Without it the kernel waits about 5.6 s for entropy; with it, a cold start reaches dockerd-ready in 1.1–1.3 s |

## Pieces

- `crates/rekcod-proto`: port numbers and the forward header (`tcp:<port>\n`), shared and unit-tested.
- `crates/rekcod-agent` (guest, static musl): vsock→docker.sock bridge, port-forward dialer, control (ping, time, shutdown, notify), and DNS stub.
- `crates/rekcod` (host): CLI (`start`, `stop`, `status`, `import`, `shell`) plus a hidden `daemon` that owns the VM, the socket proxy, the port forwarder and the clock push. It must be codesigned with `com.apple.security.virtualization`.
- `guest/`: Dockerfile that produces `Image` (decompressed from Alpine's EFI zboot vmlinuz), `initramfs` and `root.img`.
- `scripts/build.sh` builds, signs and packs the guest. `scripts/e2e.sh` runs the verification below.

## Out of scope for the MVP

UDP port forwarding, `host.docker.internal` reaching services bound to the Mac's loopback, memory reclaim, the menu bar app, sharing `/tmp`
(use `/private/tmp`), DNS over TCP, event-coherent caching (bind-mount speed), snapshots, `rekcod run`, and the brew formula.

## Done when (`scripts/e2e.sh`): all pass, 22 checks, 2026-10-11

1. `rekcod start` reaches `docker version` through the context, and the time-to-ready is printed.
2. `docker run --rm hello-world` passes, and `echo hi | docker run -i --rm alpine cat` passes (stdin half-close).
3. Compose stack: postgres (healthcheck) + nginx with a bind-mounted `index.html` and `-p 18080:80`. `curl localhost:18080` returns the file, `depends_on: service_healthy` works, and `up` again recreates nothing.
3b. Mac edits are visible in the container and fire inotify, atomic saves are readable, and deletes stay deleted.
4. `docker build` works through compose `build:` (BuildKit/bake path).
5. `docker run --platform linux/amd64 alpine uname -m` prints `x86_64`.
6. `rekcod import --from <other context>` brings over an image and a volume with its data.
7. `rekcod stop` powers off and restores the previous context.
