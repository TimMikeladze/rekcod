#!/bin/sh
# run.sh <logfile> <command...>: run a service, pause briefly if it exits so init's respawn doesn't spin.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
log=$1; shift
# boot.sh found the data disk unusable: dockerd would write into the small root disk.
if [ "${1##*/}" = dockerd ] && [ -e /run/rekcod-data-failed ]; then
  echo "rekcod: not starting dockerd: $(cat /run/rekcod-data-failed)" >/dev/console
  exec sleep 2147483647
fi
# Logs live on the data disk (dockerd's) or the 64 MB root disk: keep one old copy, at most 20 MB each.
if [ "$log" != /dev/console ] && [ -f "$log" ] && [ "$(wc -c <"$log")" -gt 20971520 ]; then
  mv "$log" "$log.1"
fi
"$@" >>"$log" 2>&1
rc=$?
echo "rekcod: $1 exited with $rc" >/dev/console
[ "$log" = /dev/console ] || tail -n 5 "$log" | sed 's/^/  | /' >/dev/console
sleep 1
