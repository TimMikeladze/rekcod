#!/bin/sh
# Boots a built guest under QEMU and checks that it comes up: the kernel mounts its root,
# boot.sh formats and mounts a new data disk, and dockerd listens. CI runs it after every
# guest build (.github/actions/guest); hosted runners can't run Virtualization.framework,
# so this is the only boot a PR or the weekly bump PR gets.
#   sudo guest/smoke.sh out/guest
# Needs root (loop mount), qemu-system-aarch64 and e2fsprogs. KVM is used on an arm64 host
# that has /dev/kvm, TCG otherwise.
#
# Boots copies: out/guest stays as built (VERSION hashes root.img). The root copy gets one
# extra inittab entry, /etc/rekcod/smoke-check.sh, which waits for dockerd, prints what the
# host checks below read, and powers off. Not covered: virtiofs, vsock, Rosetta, the
# elastic memory device. The VM suites (e2e.yml) cover those.
set -eu

[ $# -eq 1 ] || { echo "usage: $0 <guest dir>" >&2; exit 2; }
[ "$(id -u)" = 0 ] || { echo "$0: needs root (loop mount)" >&2; exit 1; }
GUEST=$(cd "$1" && pwd)
W=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-smoke.XXXXXX")
MNT=$W/mnt
cleanup() {
  mountpoint -q "$MNT" 2>/dev/null && umount "$MNT"
  rm -rf "$W"
}
trap cleanup EXIT INT TERM
log() { echo "guest-smoke: $*" >&2; }

cp "$GUEST/root.img" "$W/root.img"
truncate -s 4G "$W/data.img"
mkdir "$MNT"
mount -o loop "$W/root.img" "$MNT"
cat >"$MNT/etc/rekcod/smoke-check.sh" <<'EOF'
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
say() { echo "rekcod-smoke: $*" >/dev/console; }
i=0
until grep -q "API listen on /var/run/docker.sock" /var/lib/docker/dockerd.log 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 600 ]; then
    say "dockerd never listened"
    tail -n 30 /var/lib/docker/dockerd.log 2>/dev/null | sed 's/^/rekcod-smoke: | /' >/dev/console
    poweroff -f
  fi
  sleep 1
done
say "dockerd listening"
iptables -t nat -S PREROUTING | sed 's/^/rekcod-smoke: nat: /' >/dev/console
iptables -S INPUT | sed 's/^/rekcod-smoke: filter: /' >/dev/console
sed 's/^/rekcod-smoke: daemon.json: /' /etc/docker/daemon.json >/dev/console
echo >/dev/console
say "done"
sync
poweroff -f
EOF
chmod 0755 "$MNT/etc/rekcod/smoke-check.sh"
echo "::once:/etc/rekcod/smoke-check.sh" >>"$MNT/etc/inittab"
umount "$MNT"

if [ "$(uname -m)" = aarch64 ] && [ -w /dev/kvm ]; then
  accel="-accel kvm -cpu host"
else
  # pauth-impdef: QEMU's own pointer-auth algorithm, much faster under TCG than QARMA.
  accel="-accel tcg,thread=multi -cpu max,pauth-impdef=on"
fi
initrd=""
[ ! -f "$GUEST/initramfs" ] || initrd="-initrd $GUEST/initramfs"
# The cmdline is daemon.rs's KERNEL_CMDLINE plus rekcod.fresh=1 (the Mac just made the
# data disk), panic=-1 so a failed boot exits now, not at the timeout, and earlycon (PL011)
# for messages from before hvc0 exists.
cmdline="console=hvc0 root=/dev/vda rootfstype=ext4 modules=virtio_blk,ext4,virtio-rng rw quiet rekcod.fresh=1 panic=-1 earlycon"
log "booting ($accel)"
st=0
# shellcheck disable=SC2086 # $accel and $initrd are word lists
timeout 900 qemu-system-aarch64 -nodefaults -display none -no-reboot \
  -machine virt,gic-version=max $accel -smp 2 -m 2048 \
  -kernel "$GUEST/Image" $initrd -append "$cmdline" \
  -serial "file:$W/serial.log" \
  -device virtio-serial-pci -chardev "file,id=con,path=$W/console.log" -device virtconsole,chardev=con \
  -drive "if=none,id=root,format=raw,file=$W/root.img" -device virtio-blk-pci,drive=root \
  -drive "if=none,id=data,format=raw,file=$W/data.img" -device virtio-blk-pci,drive=data \
  -netdev user,id=net -device virtio-net-pci,netdev=net,romfile= \
  -device virtio-rng-pci || st=$?
[ "$st" != 124 ] || log "QEMU still running after 900 s"

fails=0
has() {
  if grep -q -- "$2" "$W/console.log" 2>/dev/null; then
    echo "  ok    $1"
  else
    echo "  FAIL  $1"
    fails=$((fails + 1))
  fi
}
has "data disk formatted" "rekcod-boot: .* formatting data disk"
has "data disk mounted" "rekcod-boot: .* data disk mounted"
has "boot.sh finished" "rekcod-boot: .* ready"
has "dockerd listening" "rekcod-smoke: dockerd listening"
has "host.docker.internal redirect skips eth0" "rekcod-smoke: nat: .*! -i eth0 .*--to-ports 2379"
has "eth0 can't reach the reverse listener" "rekcod-smoke: filter: .*-i eth0 .*--dport 2379 -j DROP"
has "container logs are capped" 'rekcod-smoke: daemon.json: .*"max-size": "10m"'
if grep -q "rekcod: /usr/local/bin/dockerd exited" "$W/console.log" 2>/dev/null; then
  echo "  FAIL  dockerd exited"
  fails=$((fails + 1))
fi
if [ "$fails" -gt 0 ]; then
  echo "--- serial (early boot)"
  tail -n 40 "$W/serial.log" 2>/dev/null || true
  echo "--- console"
  tail -n 80 "$W/console.log" 2>/dev/null || true
  exit 1
fi
log "ok"
