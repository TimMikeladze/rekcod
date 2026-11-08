# Sourced by boot.sh. The host tests run it on scratch images with stub tools
# (daemon.rs, `data_disk_is_formatted_only_when_the_mac_just_made_it`).

is_ext4() { blkid "$1" 2>/dev/null | grep -q 'TYPE="ext4"'; }
blank() { [ "$(head -c 4194304 "$1" | tr -d '\000' | head -c 1 | wc -c)" -eq 0 ]; }
on_cmdline() { case " $2 " in *" $1 "*) return 0 ;; esac; return 1; }

# data_disk_ready DEV CMDLINE: 0 once DEV is a readable ext4 filesystem. DEV is formatted
# only when the Mac created it and dockerd never answered on it (rekcod.fresh=1) and its
# first 4 MiB are still zeros. Zeros alone prove nothing: damage there leaves the data
# behind it. Any other disk gets e2fsck from a backup superblock (all of them, with -y,
# under `rekcod start --repair`) and is never formatted.
data_disk_ready() {
  local dev=$1 cmdline=$2 out=${REKCOD_CONSOLE:-/dev/console} b
  is_ext4 "$dev" && return 0
  if on_cmdline rekcod.fresh=1 "$cmdline" && blank "$dev"; then
    log "formatting data disk"
    mkfs.ext4 -q -L rekcod-data -m 0 -E lazy_itable_init=1,lazy_journal_init=1 "$dev"
    is_ext4 "$dev"
    return
  fi
  log "data disk has no readable ext4 superblock; trying the backup"
  e2fsck -p -b 32768 -B 4096 "$dev" >"$out" 2>&1
  if ! is_ext4 "$dev" && on_cmdline rekcod.fsck=yes "$cmdline"; then
    for b in 98304 163840 229376 294912 819200 884736 1605632 2654208 4096000 7962624; do
      log "trying backup superblock $b"
      e2fsck -fy -b $b -B 4096 "$dev" >"$out" 2>&1
      is_ext4 "$dev" && break
    done
  fi
  is_ext4 "$dev"
}
