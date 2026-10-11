#!/bin/sh
# rekcod guest boot (busybox init sysinit). Ordering matters; keep it fast.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
log() { echo "rekcod-boot: [$(cut -d' ' -f1 /proc/uptime)s] $*" >/dev/console; }

mountpoint -q /proc || mount -t proc proc /proc
log "init"
mountpoint -q /sys || mount -t sysfs sysfs /sys
mountpoint -q /dev || mount -t devtmpfs devtmpfs /dev
mkdir -p /dev/pts /dev/shm
mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
mount -t tmpfs -o mode=1777,nosuid,nodev shm /dev/shm
mount -t tmpfs -o mode=755,nosuid,nodev tmpfs /run
mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /tmp
grep -q " / ext4 rw" /proc/mounts || mount -o remount,rw /
mount -t cgroup2 -o nsdelegate cgroup2 /sys/fs/cgroup
for c in cpuset cpu io memory pids hugetlb; do echo "+$c" >/sys/fs/cgroup/cgroup.subtree_control 2>/dev/null; done
hostname rekcod

modprobe -a virtio_net virtio_rng virtio_balloon vmw_vsock_virtio_transport virtiofs \
  overlay br_netfilter binfmt_misc 2>/dev/null

echo 1048576 >/proc/sys/fs/inotify/max_user_watches
echo 8192 >/proc/sys/fs/inotify/max_user_instances
echo 262144 >/proc/sys/vm/max_map_count
echo 1 >/proc/sys/net/ipv4/ip_forward

# Network (VZ NAT). udhcpc's default script writes the address, route and resolv.conf.
log "modules loaded"
ip link set lo up
ip link set eth0 up
udhcpc -i eth0 -f -q -n -t 30 -T 1 -A 1 >/dev/null 2>&1 || log "DHCP failed"
log "network up"

# Data disk: formatted on first boot, holds all docker state.
if ! blkid /dev/vdb 2>/dev/null | grep -q 'TYPE="ext4"'; then
  log "formatting data disk"
  mkfs.ext4 -q -L rekcod-data -m 0 -E lazy_itable_init=1,lazy_journal_init=1 /dev/vdb
fi
mount -o noatime /dev/vdb /var/lib/docker || log "data disk mount failed"
log "data disk mounted"

# Mac directories at the same paths as on the Mac, so bind mounts need no translation.
# "rekcod" = patched virtiofs with host-driven invalidation (docs/phase2.md);
# fall back to stock behaviour if the module doesn't know the option.
if mount -t virtiofs -o rekcod mac /mnt/mac 2>/dev/null || mount -t virtiofs mac /mnt/mac; then
  for d in Users Volumes private; do
    [ -d "/mnt/mac/$d" ] && mount --bind "/mnt/mac/$d" "/$d"
  done
  [ -d /private/var/folders ] && mount --bind /private/var/folders /var/folders
else
  log "mac share unavailable"
fi

# Rosetta for linux/amd64 binaries (F: interpreter opened now, usable inside containers).
if mount -t virtiofs rosetta /mnt/rosetta 2>/dev/null; then
  mountpoint -q /proc/sys/fs/binfmt_misc || mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc
  [ -e /proc/sys/fs/binfmt_misc/rosetta ] || printf '%s\n' \
    ':rosetta:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00:\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:/mnt/rosetta/rosetta:CF' \
    >/proc/sys/fs/binfmt_misc/register
fi

# DNS: dnsmasq on eth0 answers host.docker.internal with the Mac (vmnet gateway) and
# forwards everything else to rekcod-agent's stub, which resolves via the Mac.
gw=$(ip route show default | awk '{print $3; exit}')
ip4=$(ip -4 -o addr show dev eth0 | awk '{split($4, a, "/"); print a[1]; exit}')
if [ -n "$ip4" ]; then
  {
    echo "listen-address=$ip4"
    echo "bind-interfaces"
    echo "no-resolv"
    echo "no-hosts"
    echo "server=127.0.0.1#5353"
    echo "address=/host.docker.internal/$gw"
    echo "address=/gateway.docker.internal/$gw"
    echo "cache-size=1000"
  } >/etc/dnsmasq.conf
  echo "nameserver $ip4" >/etc/resolv.conf
fi

if [ -n "$gw" ]; then
  echo "{\"host-gateway-ips\": [\"$gw\"]}" >/etc/docker/daemon.json
else
  echo "{}" >/etc/docker/daemon.json
fi
log "ready (ip ${ip4:-none}, gateway ${gw:-none})"
