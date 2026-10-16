#!/usr/bin/env bash
# VM lifecycle and data safety against a VM of its own (REKCOD_HOME=~/.rke2l): first boot,
# kill -9, stale pid files, concurrent starts, a damaged data disk, --repair, port
# shadowing, the login agent's exit codes. Leaves your main VM alone.
#
# Needs alpine:3.22 in the current docker context (it's copied in; the test VM may not
# have internet), and a guest: REKCOD_GUEST_DIR, else out/guest.
#   REKCOD=/path/to/signed/rekcod scripts/e2e-lifecycle.sh
set -uo pipefail
cd "$(dirname "$0")/.."
R=${REKCOD:-$PWD/target/release/rekcod}
export REKCOD_HOME=$HOME/.rke2l
export REKCOD_GUEST_DIR=${REKCOD_GUEST_DIR:-$PWD/out/guest}
D="docker -H unix://$REKCOD_HOME/run/docker.sock"
fails=0
check() {
  local name=$1; shift
  if out=$("$@" 2>&1); then echo "  ok    $name"; else echo "  FAIL  $name"; echo "$out" | tail -3 | sed 's/^/        /'; fails=$((fails + 1)); fi
}
cleanup() {
  "$R" stop --no-switch --force >/dev/null 2>&1
  [[ -n ${listener:-} ]] && kill "$listener" 2>/dev/null
  rm -rf "$REKCOD_HOME"
}
trap cleanup EXIT
cleanup
start() { "$R" start --no-switch --cpus 2 --memory 2 --disk 8 "$@"; }

echo "first boot"
check "boots" start
check "formatted only because the disk was new" grep -aq "formatting data disk" "$REKCOD_HOME/logs/console.log"
check "data.fresh is gone once dockerd answered" test ! -e "$REKCOD_HOME/vm/data.fresh"
check "settings saved" grep -q '"memory_gib": 2' "$REKCOD_HOME/config.json"
check "VM disks excluded from Time Machine" sh -c "tmutil isexcluded '$REKCOD_HOME/vm' | grep -q Excluded"
check "run/ is private" sh -c "stat -f %Lp '$REKCOD_HOME/run' | grep -qx 700"
docker save alpine:3.22 | $D load >/dev/null
$D volume create keep >/dev/null
$D run --rm -v keep:/k alpine:3.22 sh -c 'echo precious > /k/f'
check "a stalled reader gets all 200 MiB" sh -c "$D run --rm alpine:3.22 dd if=/dev/zero bs=1M count=200 2>/dev/null | (sleep 8; wc -c) | grep -q 209715200"

echo "crashes"
kill -9 "$(cat "$REKCOD_HOME/run/daemon.pid")"; sleep 1
check "a stale pid file doesn't count as running" sh -c "'$R' status | grep -q 'not running'"
"$R" start --no-switch >/dev/null 2>&1 &
"$R" start --no-switch >/dev/null 2>&1 &
wait
check "two starts at once boot one VM" sh -c "test \$(pgrep -f '$R daemon' | wc -l) -eq 1"
check "data survives kill -9" sh -c "$D run --rm -v keep:/k alpine:3.22 cat /k/f | grep -q precious"

echo "damaged data disk"
"$R" stop --no-switch >/dev/null
check "a clean stop is recorded" grep -q clean "$REKCOD_HOME/run/last-exit"
dd if=/dev/zero of="$REKCOD_HOME/vm/data.img" bs=1024 seek=1 count=1 conv=notrunc 2>/dev/null
check "a zeroed superblock is rebuilt from the backup" start
check "and the data is still there" sh -c "$D run --rm -v keep:/k alpine:3.22 cat /k/f | grep -q precious"
"$R" stop --no-switch >/dev/null
dd if=/dev/urandom of="$REKCOD_HOME/vm/data.img" bs=1M count=200 conv=notrunc 2>/dev/null
check "a disk past the first backup isn't formatted; start says so" sh -c "! '$R' start --no-switch 2>&1 | tee /dev/stderr | grep -q ready"
check "the error names --repair" sh -c "'$R' status >/dev/null; grep -aq 'not formatting it' '$REKCOD_HOME/logs/console.log'"
"$R" stop --no-switch --force >/dev/null 2>&1
check "start --repair brings it back" start --repair
check "dockerd answers after the repair" $D info

echo "ports"
port=18555
python3 -c "import socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('0.0.0.0', $port)); s.listen(); time.sleep(120)" &
listener=$!
sleep 1
$D run -d --name shadow -p 127.0.0.1:$port:80 alpine:3.22 sleep 300 >/dev/null
sleep 2
check "a port the Mac already serves isn't taken" sh -c "'$R' status --json | grep -A3 '\"port\": $port' | grep -q error"
$D rm -f shadow >/dev/null

echo "login agent"
"$R" start --no-switch --foreground >/dev/null 2>&1 &
fg=$!
sleep 3
kill -9 "$(cat "$REKCOD_HOME/run/daemon.pid")"
wait $fg
check "a crash makes --foreground exit 1 (launchd boots it again)" test $? -eq 1
"$R" start --no-switch >/dev/null 2>&1
"$R" start --no-switch --foreground >/dev/null 2>&1 &
fg=$!
sleep 3
"$R" stop --no-switch >/dev/null
wait $fg
check "rekcod stop makes --foreground exit 0" test $? -eq 0

echo
if [[ $fails -gt 0 ]]; then
  echo "$fails lifecycle check(s) failed"
  exit 1
fi
echo "all lifecycle checks passed"
