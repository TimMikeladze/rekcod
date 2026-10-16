# Building the guest without Docker

The guest image (`out/guest`: `Image`, `[initramfs]`, `root.img`, `VERSION`) used to come
from `docker buildx build guest/Dockerfile`. Nothing in it needed Docker: apk installs, a
tarball download, a kernel compile and `mke2fs -d`. `guest/build.sh` now does the same
stages with plain chroots, so building rekcod needs no Docker engine and CI needs no buildx.

## How it works

`guest/build.sh <agent binary> <out dir>` runs as root on any Linux arm64 machine with
`chroot`, `mount`, `tar` and `curl` or `wget`.

1. Downloads the Alpine minirootfs for `$ALPINE_RELEASE` once into `$WORK`, checked
   against `MINIROOTFS_SHA256`.
2. `rootfs`: a fresh minirootfs chroot, `apk add` the runtime packages, the static
   dockerd tarball (checked against `DOCKER_SHA256`), `rekcod-agent` and `guest/rootfs/`.
3. `tools`: a second chroot with the kernel build deps and e2fsprogs. With `KERNEL=custom`
   (default) the kernel is compiled in it from the kernel.org tarball for
   `linux-virt=$LINUX_VIRT_VERSION` (checked against `LINUX_SHA256`). With `KERNEL=alpine` the stock `linux-virt` is
   unpacked inside a copy of `rootfs` instead, as before.
4. `pack`: in `tools`, the same `mke2fs` sizing and `VERSION` hash as the Dockerfile's
   `pack` stage.

The compiled kernel (`kout/`) is cached in `$REKCOD_KERNEL_CACHE` under a key hashed from
the build settings, the pinned `linux-virt` version and tarball hash, `guest/kernel/`,
`guest/fuse/` and the kernel section of `guest/build.sh`. It's the replacement for buildx's
layer cache. CI keeps that directory with `actions/cache`, keyed on the same files plus
`guest/versions.env`.

Settings are env vars: `KERNEL=custom`, `KERNEL_REPO=`, `FUSE_PATCH=1`, and the pins in
`guest/versions.env` (`ALPINE_RELEASE`, `DOCKER_VERSION`, `LINUX_VIRT_VERSION` and a sha256
for each download). Overriding a version without its sha256 fails the build on purpose.

## Bumping pinned versions

Every download is checked against a sha256 in `guest/versions.env`, and the build stops on
a mismatch. Alpine keeps only the newest `linux-virt` in a release branch, so the build also
stops when Alpine ships a new kernel: `apk add linux-virt=<old>` can't find the package.
To bump:

```sh
# newest linux-virt in the branch
curl -fsSL https://dl-cdn.alpinelinux.org/alpine/v3.22/main/aarch64/APKINDEX.tar.gz | tar -xzO APKINDEX \
  | awk 'BEGIN { RS = "" } /\nP:linux-virt\n/' | grep '^V:'
# kernel.org's hash for that version (6.12.N, without the -rN suffix)
curl -fsSL https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc | grep 'linux-6.12.N.tar.xz'
# minirootfs file and sha256
curl -fsSL https://dl-cdn.alpinelinux.org/alpine/v3.22/releases/aarch64/latest-releases.yaml
# docker publishes no hash: download it and hash it yourself
curl -fsSLO https://download.docker.com/linux/static/stable/aarch64/docker-29.9.0.tgz && shasum -a 256 docker-29.9.0.tgz
```

Edit `guest/versions.env` and rebuild. A new `linux-virt` version changes the kernel cache
key, so the kernel recompiles. The runtime `apk add` packages (iptables, dnsmasq and so on)
aren't version-pinned; apk checks their signatures against the Alpine keys in the
minirootfs.

## Where it runs

- CI (`ubuntu-24.04-arm`): `sudo guest/build.sh ...`.
- A Mac: `scripts/build.sh` runs it inside the running rekcod VM through `rekcod shell`.
  The repo is visible at the same path there. Work files go to the VM's data disk
  (`/var/lib/docker/.rekcod-guest-build`, a few GB with the kernel tree). The VM has to
  be running (`rekcod start`). A fresh checkout boots the released guest first.

`rekcod shell` reaches the VM through rekcod's own dockerd, so no Docker Desktop or other
engine is involved.
