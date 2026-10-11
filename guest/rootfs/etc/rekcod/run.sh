#!/bin/sh
# run.sh <logfile> <command...>: run a service, pause briefly if it exits so init's respawn doesn't spin.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
log=$1; shift
"$@" >>"$log" 2>&1
rc=$?
echo "rekcod: $1 exited with $rc" >/dev/console
[ "$log" = /dev/console ] || tail -n 5 "$log" | sed 's/^/  | /' >/dev/console
sleep 1
