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
# The Mac side may have grown data.img (`rekcod start --disk`); ext4 grows online.
resize2fs /dev/vdb >/dev/null 2>&1 || log "data disk resize failed"
log "data disk mounted"

# Mac directories at the same paths as on the Mac, so bind mounts need no translation.
# "rekcod" = patched virtiofs with host-driven invalidation (docs/phase2.md);
# fall back to stock behaviour if the module doesn't know the option.
if mount -t virtiofs -o rekcod mac /mnt/mac 2>/dev/null || mount -t virtiofs mac /mnt/mac; then
  for d in Users Volumes private; do
    [ -d "/mnt/mac/$d" ] && mount --bind "/mnt/mac/$d" "/$d"
  done
  [ -d /private/var/folders ] && mount --bind /private/var/folders /var/folders
  # Mac /tmp is /private/tmp: `-v /tmp/x:/x` must mean the Mac's /tmp/x (as on
  # Docker Desktop/OrbStack). Guest services keep their state under /run and /var/lib.
  [ -d /private/tmp ] && mount --bind /private/tmp /tmp
  # Sandbox VM (docs/sandbox.md): the share holds only the project directory,
  # mounted at its real Mac path (base64 on the cmdline, paths may have spaces).
  proj=$(sed -n 's/.*rekcod\.project=\([^ ]*\).*/\1/p' /proc/cmdline)
  if [ -n "$proj" ] && [ -d /mnt/mac/project ]; then
    dir=$(echo "$proj" | base64 -d)
    mkdir -p "$dir" && mount --bind /mnt/mac/project "$dir" && log "project $dir"
  fi
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

# Sandbox: internet only. Nothing on the Mac (vmnet gateway, the Mac's LAN
# addresses) or the local network; DNS still works (it goes over vsock).
# DOCKER-USER exists before dockerd starts, and dockerd keeps it first.
if grep -q 'rekcod.sandbox=1' /proc/cmdline; then
  iptables -N DOCKER-USER 2>/dev/null
  for net in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 100.64.0.0/10; do
    iptables -A OUTPUT -o eth0 -d "$net" -j REJECT
    iptables -A DOCKER-USER -o eth0 -d "$net" -j REJECT
  done
  # Agent sandbox allowlist (docs/agent-sandboxes.md): rekcod-agent adds an ACCEPT to
  # REKCOD-ALLOW for each address an allowed name resolves to; everything else is rejected.
  if grep -q 'rekcod.allow=' /proc/cmdline; then
    iptables -N REKCOD-ALLOW
    for chain in OUTPUT DOCKER-USER; do
      iptables -A "$chain" -o eth0 -j REKCOD-ALLOW
      iptables -A "$chain" -o eth0 -j REJECT
    done
    log "sandbox network: allowlist"
  fi
  # rekcod.reach=IP:PORT,...: exact TCP holes through the block above (an app instance's
  # RustFS, docs/rustfs.md). Inserted first, so they win over the REJECTs and the allowlist.
  reach=$(tr ' ' '\n' </proc/cmdline | sed -n 's/^rekcod\.reach=//p')
  for hp in $(echo "$reach" | tr ',' ' '); do
    for chain in OUTPUT DOCKER-USER; do
      iptables -I "$chain" 1 -o eth0 -d "${hp%:*}" -p tcp --dport "${hp##*:}" -j ACCEPT
    done
    log "sandbox network: may reach $hp"
  done
  iptables -A DOCKER-USER -j RETURN
  ip6tables -A OUTPUT -o eth0 -j REJECT
  ip6tables -A FORWARD -o eth0 -j REJECT
  log "sandbox network: internet only"
# host.docker.internal is the gateway address. Its TCP goes to rekcod-agent,
# which has the daemon connect to 127.0.0.1:<port> on the Mac, so services
# listening only on the Mac's loopback are reachable too.
elif [ -n "$gw" ]; then
  iptables -t nat -I PREROUTING -d "$gw" -p tcp -j REDIRECT --to-ports 2379
  iptables -t nat -I OUTPUT -d "$gw" -p tcp -j REDIRECT --to-ports 2379
fi

# Build cache would otherwise grow until the data disk is full.
gc='"builder": {"gc": {"enabled": true, "defaultMaxUsedSpace": "20GB"}}'
if [ -n "$gw" ]; then
  echo "{\"host-gateway-ips\": [\"$gw\"], $gc}" >/etc/docker/daemon.json
else
  echo "{$gc}" >/etc/docker/daemon.json
fi
log "ready (ip ${ip4:-none}, gateway ${gw:-none})"
