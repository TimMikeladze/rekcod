# Building the guest without Docker

The guest image (`out/guest`: `Image`, `[initramfs]`, `root.img`, `VERSION`) used to come
from `docker buildx build guest/Dockerfile`. Nothing in it needed Docker: apk installs, a
tarball download, a kernel compile and `mke2fs -d`. `guest/build.sh` now does the same
stages with plain chroots, so building rekcod needs no Docker engine and CI needs no buildx.

## How it works

`guest/build.sh <agent binary> <out dir>` runs as root on any Linux arm64 machine with
`chroot`, `mount`, `tar` and `curl` or `wget`.

1. Downloads the Alpine minirootfs for `$ALPINE` (sha256 checked against
   `latest-releases.yaml`) once into `$WORK`.
2. `rootfs`: a fresh minirootfs chroot, `apk add` the runtime packages, the static
   dockerd tarball, `rekcod-agent` and `guest/rootfs/`.
3. `tools`: a second chroot with the kernel build deps and e2fsprogs. With `KERNEL=custom`
   (default) the kernel is compiled in it. With `KERNEL=alpine` the stock `linux-virt` is
   unpacked inside a copy of `rootfs` instead, as before.
4. `pack`: in `tools`, the same `mke2fs` sizing and `VERSION` hash as the Dockerfile's
   `pack` stage.

The compiled kernel (`kout/`) is cached in `$REKCOD_KERNEL_CACHE` under a key hashed from
the build settings, `guest/kernel/`, `guest/fuse/` and the kernel section of `guest/build.sh`. It's the
replacement for buildx's layer cache. CI keeps that directory with `actions/cache`.

Settings are env vars with the Dockerfile's old defaults: `ALPINE=3.22`,
`DOCKER_VERSION=29.9.0`, `KERNEL=custom`, `KERNEL_REPO=`, `FUSE_PATCH=1`.

## Where it runs

- CI (`ubuntu-24.04-arm`): `sudo guest/build.sh ...`.
- A Mac: `scripts/build.sh` runs it inside the running rekcod VM through `rekcod shell`.
  The repo is visible at the same path there. Work files go to the VM's data disk
  (`/var/lib/docker/.rekcod-guest-build`, a few GB with the kernel tree). The VM has to
  be running (`rekcod start`). A fresh checkout boots the released guest first.

`rekcod shell` reaches the VM through rekcod's own dockerd, so no Docker Desktop or other
engine is involved.
