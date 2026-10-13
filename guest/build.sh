#!/bin/sh
# rekcod guest: kernel + Alpine userland + upstream static dockerd + rekcod-agent,
# built in plain chroots (docs/guest-build.md). Runs as root on Linux arm64.
#   guest/build.sh <rekcod-agent binary> <out dir>
set -eu

ALPINE=${ALPINE:-3.22}
DOCKER_VERSION=${DOCKER_VERSION:-29.9.0}
# custom = our kernel (docs/custom-kernel.md): kernel.org sources for the version
# Alpine ships, Alpine's config + guest/kernel/rekcod.config, fuse patch in-tree,
# no initramfs. alpine = Alpine's stock linux-virt with out-of-tree fuse modules.
KERNEL=${KERNEL:-custom}
# Kernel package source: empty = the Alpine release's linux-virt; or e.g.
# https://dl-cdn.alpinelinux.org/alpine/edge/main for a newer one.
KERNEL_REPO=${KERNEL_REPO:-}
# 0 = stock fuse/virtiofs (bisecting; boot.sh falls back to uncached sharing)
FUSE_PATCH=${FUSE_PATCH:-1}
WORK=${REKCOD_GUEST_WORK:-/var/tmp/rekcod-guest}
KCACHE=${REKCOD_KERNEL_CACHE:-$WORK/kernel-cache}

[ $# -eq 2 ] || { echo "usage: $0 <rekcod-agent> <out dir>" >&2; exit 2; }
[ "$(id -u)" = 0 ] || { echo "$0: needs root (chroot, mount)" >&2; exit 1; }
[ "$(uname -m)" = aarch64 ] || { echo "$0: needs an arm64 Linux machine" >&2; exit 1; }
AGENT=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
OUT=$2
G=$(cd "$(dirname "$0")" && pwd)
[ -f "$AGENT" ] || { echo "$0: no agent at $AGENT" >&2; exit 1; }
[ "$KERNEL" = custom ] || [ "$KERNEL" = alpine ] || { echo "KERNEL must be custom or alpine" >&2; exit 2; }

log() { echo "guest-build: $*" >&2; }
fetch() { if command -v curl >/dev/null; then curl -fsSL -o "$2" "$1"; else wget -qO "$2" "$1"; fi; }

MOUNTS=""
cleanup() { for m in $MOUNTS; do umount -l "$m" 2>/dev/null || true; done; }
trap cleanup EXIT INT TERM

# --- minirootfs ----------------------------------------------------------------
mkdir -p "$WORK" "$KCACHE"
base=https://dl-cdn.alpinelinux.org/alpine/v$ALPINE/releases/aarch64
fetch "$base/latest-releases.yaml" "$WORK/releases.yaml"
set -- $(awk '/flavor:/ { m = ($2 == "alpine-minirootfs") } m && /file:/ { f = $2 } m && /sha256:/ { print f, $2; exit }' "$WORK/releases.yaml")
MINI=$WORK/$1
if [ ! -f "$MINI" ] || [ "$(sha256sum "$MINI" | cut -d' ' -f1)" != "$2" ]; then
  log "downloading $1"
  fetch "$base/$1" "$MINI.part"
  [ "$(sha256sum "$MINI.part" | cut -d' ' -f1)" = "$2" ] || { log "sha256 mismatch for $1"; exit 1; }
  mv "$MINI.part" "$MINI"
fi

# A fresh chroot from the minirootfs at $1.
mkroot() {
  rm -rf "$1" && mkdir -p "$1"
  tar -xzf "$MINI" -C "$1"
}
# Run "$2" with sh -euc inside chroot $1 (proc, dev and DNS from the host).
inroot() {
  r=$1
  mount -t proc proc "$r/proc" && MOUNTS="$r/proc $MOUNTS"
  mount --bind /dev "$r/dev" && MOUNTS="$r/dev $MOUNTS"
  had_resolv=0; [ -e "$r/etc/resolv.conf" ] && had_resolv=1
  cp -L /etc/resolv.conf "$r/etc/resolv.conf.host" && mv "$r/etc/resolv.conf.host" "$r/etc/resolv.conf"
  st=0; chroot "$r" /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/root \
    ALPINE="$ALPINE" DOCKER_VERSION="$DOCKER_VERSION" KERNEL_REPO="$KERNEL_REPO" FUSE_PATCH="$FUSE_PATCH" \
    /bin/sh -euc "$2" || st=$?
  [ "$had_resolv" = 1 ] || rm -f "$r/etc/resolv.conf"
  umount "$r/dev" "$r/proc"
  MOUNTS=$(echo "$MOUNTS" | sed "s#$r/dev ##; s#$r/proc ##")
  return $st
}
# Copy directory $1's contents into $2, owned by root (the checkout isn't).
copyroot() {
  mkdir -p "$2" && cp -R "$1/." "$2/"
  (cd "$1" && find . -print) | while IFS= read -r p; do chown -h 0:0 "$2/$p"; done
}

# --- rootfs ----------------------------------------------------------------------
log "rootfs"
R=$WORK/rootfs
mkroot "$R"
inroot "$R" '
apk add --no-cache kmod e2fsprogs e2fsprogs-extra iptables ip6tables dnsmasq ca-certificates
wget -qO /tmp/docker.tgz "https://download.docker.com/linux/static/stable/aarch64/docker-${DOCKER_VERSION}.tgz"
tar -xzf /tmp/docker.tgz -C /tmp
cd /tmp/docker
install -m 0755 dockerd containerd containerd-shim-runc-v2 runc docker-proxy docker-init /usr/local/bin/
rm -rf /tmp/docker /tmp/docker.tgz
'
install -m 0755 -o 0 -g 0 "$AGENT" "$R/usr/local/bin/rekcod-agent"
copyroot "$G/rootfs" "$R"
chmod 0755 "$R"/etc/rekcod/*.sh
mkdir -p "$R/mnt/mac" "$R/mnt/rosetta" "$R/Users" "$R/Volumes" "$R/private" "$R/var/folders" \
  "$R/var/lib/docker" "$R/etc/docker"

# --- tools: kernel build deps + e2fsprogs -----------------------------------------
log "tools"
T=$WORK/tools
mkroot "$T"
inroot "$T" '
apk add --no-cache build-base bc bison flex perl openssl openssl-dev elfutils-dev linux-headers \
  gmp-dev mpfr-dev mpc1-dev xz kmod diffutils findutils bash e2fsprogs coreutils
'

# --- kernel: leaves $KOUT/Image (raw arm64), $KOUT/lib/modules and, only if the
# kernel needs one, $KOUT/initramfs ---------------------------------------------------
# Keyed on the kernel steps only (this section of build.sh), so rootfs edits don't recompile.
key=$( { echo "$ALPINE $KERNEL $KERNEL_REPO $FUSE_PATCH"; sed -n '/^# --- kernel/,/^# --- pack/p' "$G/build.sh"
         find "$G/kernel" "$G/fuse" -type f | LC_ALL=C sort | xargs cat; } | sha256sum | cut -c1-16)
KOUT=$KCACHE/$key
if [ -f "$KOUT/Image" ]; then
  log "kernel: cached ($key)"
elif [ "$KERNEL" = custom ]; then
  log "kernel: custom ($key), compiling"
  rm -rf "$T/src" "$T/patch" "$T/kout"
  copyroot "$G/fuse" "$T/patch"
  inroot "$T" '
apk add --no-cache ${KERNEL_REPO:+--repository "$KERNEL_REPO"} linux-virt
KREL=$(ls /lib/modules); KV=${KREL%%-*}
wget -qO /tmp/linux.tar.xz "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KV.tar.xz"
mkdir /src && tar -xJf /tmp/linux.tar.xz -C /src --strip-components=1
rm /tmp/linux.tar.xz && cp /boot/config-*-virt /src/alpine.config
'
  install -m 0644 -o 0 -g 0 "$G/kernel/rekcod.config" "$T/src/rekcod.config"
  inroot "$T" '
cd /src
if [ "$FUSE_PATCH" = 1 ]; then (cd fs/fuse && patch -p1 < /patch/rekcod-fuse.patch && cp /patch/rekcod.c .); fi
printf "CONFIG_LOCALVERSION=\"-rekcod\"\n# CONFIG_LOCALVERSION_AUTO is not set\n" >>rekcod.config
ARCH=arm64 scripts/kconfig/merge_config.sh -m alpine.config rekcod.config >/dev/null
make ARCH=arm64 olddefconfig >/dev/null
# merge_config only warns; a fragment line that did not stick is a build error
# (an "is not set" may vanish with its parent menu; it just must not be y/m)
bad=$( { grep -E "^CONFIG_[A-Z0-9_]+=" rekcod.config | while read -r l; do
           grep -qxF "$l" .config || echo "$l"; done
         sed -n "s/^# \(CONFIG_[A-Z0-9_]*\) is not set$/\1/p" rekcod.config | while read -r s; do
           ! grep -qE "^$s=[ym]$" .config || echo "$s"; done; } )
[ -z "$bad" ] || { echo "kernel config did not take:"; echo "$bad"; exit 1; }
make ARCH=arm64 -j"$(nproc)" -s Image modules
mkdir -p /kout
make ARCH=arm64 -s modules_install INSTALL_MOD_PATH=/kout INSTALL_MOD_STRIP=1
rm -f /kout/lib/modules/*/build /kout/lib/modules/*/source
cp arch/arm64/boot/Image /kout/Image && cp .config /kout/config
'
  rm -rf "$KOUT.part" && mv "$T/kout" "$KOUT.part" && mv "$KOUT.part" "$KOUT"
  rm -rf "$T/src" "$T/patch"
else
  log "kernel: alpine linux-virt ($key)"
  A=$WORK/alpine-kernel
  rm -rf "$A" && cp -a "$R" "$A"
  copyroot "$G/fuse" "$A/patch"
  inroot "$A" '
apk add --no-cache build-base xz ${KERNEL_REPO:+--repository "$KERNEL_REPO"} linux-virt
V=$(apk list -I linux-virt 2>/dev/null | sed -E "s/^linux-virt-([^ ]+) .*/\1/")
apk add --no-cache ${KERNEL_REPO:+--repository "$KERNEL_REPO"} "linux-virt-dev=$V"
KREL=$(ls /lib/modules); KV=${KREL%%-*}
wget -qO /tmp/linux.tar.xz "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KV.tar.xz"
mkdir /src && tar -xJf /tmp/linux.tar.xz -C /src --strip-components=2 "linux-$KV/fs/fuse"
rm /tmp/linux.tar.xz
D="/lib/modules/$KREL/kernel/fs/fuse"
if [ "$FUSE_PATCH" = 1 ]; then cd /src/fuse
  patch -p1 < /patch/rekcod-fuse.patch && cp /patch/rekcod.c .
  make -s -C "/usr/src/linux-headers-$KREL" M="$PWD" modules
  for m in fuse virtiofs cuse; do gzip -9 -c "$m.ko" > "$D/$m.ko.gz"; done
  depmod -a "$KREL"; fi
# the initramfs autoloads virtiofs for the share device; it must carry ours
mkinitfs -o /boot/initramfs-virt "$KREL"
# Alpine ships an EFI zboot vmlinuz; Virtualization.framework wants the raw arm64 Image.
mkdir -p /kout/lib; k=/boot/vmlinuz-virt
off=$(od -An -t u4 -j 8 -N 4 "$k" | tr -d " "); len=$(od -An -t u4 -j 12 -N 4 "$k" | tr -d " ")
tail -c +$((off + 1)) "$k" | head -c "$len" | gunzip > /kout/Image
cp /boot/initramfs-virt /kout/initramfs; cp /boot/config-*-virt /kout/config
cp -a /lib/modules /kout/lib/
'
  rm -rf "$KOUT.part" && mv "$A/kout" "$KOUT.part" && mv "$KOUT.part" "$KOUT"
  rm -rf "$A"
fi

# --- pack ------------------------------------------------------------------------
log "pack"
rm -rf "$T/rootfs" "$T/kout" "$T/out"
mv "$R" "$T/rootfs"
cp -a "$KOUT" "$T/kout"
inroot "$T" '
mkdir /out; rm -rf /rootfs/boot/* /rootfs/lib/modules
# the cache may have come back owned by whoever restored it (CI)
cp -a /kout/lib/modules /rootfs/lib/ && chown -R 0:0 /rootfs/lib/modules; cp /kout/Image /out/Image
[ "$(dd if=/out/Image bs=1 skip=56 count=4 2>/dev/null)" = "ARMd" ]
[ ! -f /kout/initramfs ] || cp /kout/initramfs /out/initramfs
# content + 25% + 64 MB: the root only takes logs and /etc writes; docker
# state is on the data disk. The release tarball compresses the rest away.
mb=$(( $(du -sm /rootfs | cut -f1) * 5 / 4 + 64 ))
mke2fs -q -t ext4 -L rekcod-root -d /rootfs /out/root.img "${mb}M"
cat /out/Image $( [ -f /out/initramfs ] && echo /out/initramfs ) /out/root.img | sha256sum | cut -c1-16 > /out/VERSION
'
mkdir -p "$OUT"
rm -f "$OUT/Image" "$OUT/initramfs" "$OUT/root.img" "$OUT/VERSION"
cp "$T"/out/* "$OUT/"
rm -rf "$T/rootfs" "$T/kout" "$T/out"
log "done: $OUT ($(cat "$OUT/VERSION"))"
