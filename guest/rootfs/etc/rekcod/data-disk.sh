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

# orphan_file_ok DEV: 0 unless DEV has the orphan_file feature but its inode is free and
# turning the feature off failed. e2fsck 1.47.4 -fy from a backup superblock clears that
# inode (12) with the other "garbage" and keeps the feature, and later checks call the disk
# clean. The kernel then refuses to mount it or gives inode 12 to a new file, and the next
# unclean stop ends in e2fsck exit 4. Without the feature ext4 keeps orphans in the
# superblock's list, as before 1.47; e2fsck -fy fixes the inode count tune2fs leaves.
# Only run it right after e2fsck -fy (--repair): after a -p recovery from a backup the
# bitmaps can still fail their checksums, debugfs reads inode 12 as free and tune2fs refuses.
orphan_file_ok() {
  local dev=$1 out=${REKCOD_CONSOLE:-/dev/console} ino
  ino=$(dumpe2fs -h "$dev" 2>/dev/null | sed -n 's/^Orphan file inode: *//p')
  [ -z "$ino" ] && return 0
  debugfs -R "testi <$ino>" "$dev" 2>&1 | grep -q 'is not in use' || return 0
  log "the data disk's orphan file (inode $ino) is gone; turning orphan_file off"
  tune2fs -O ^orphan_file "$dev" >"$out" 2>&1 || return 1
  e2fsck -fy "$dev" >"$out" 2>&1
  [ $? -lt 4 ]
}
