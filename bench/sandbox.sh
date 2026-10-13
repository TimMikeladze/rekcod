#!/bin/sh
# Agent sandbox timings (docs/agent-sandboxes.md, Verification). Needs a golden disk
# (`rekcod sandbox golden build`). Creates and removes its own sandboxes only.
set -eu
B=${REKCOD:-rekcod}
N=${N:-5}
CONC=${CONC:-16}
proj=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-bench-XXXXXX")
echo 'print(1)' >"$proj/app.py"
now() { perl -MTime::HiRes=time -e 'printf "%.3f", time'; }
p50() { sort -n | awk '{a[NR]=$1} END {printf "p50 %.2fs  max %.2fs  (n=%d)\n", a[int((NR+1)/2)], a[NR], NR}'; }

printf 'create to first exec  '
for _ in $(seq "$N"); do
  t=$(now); id=$($B sandbox create "$proj" --image python:3.13-slim 2>/dev/null)
  $B sandbox exec "$id" -- python3 app.py >/dev/null; echo "$(now) - $t" | bc
  $B sandbox rm "$id" >/dev/null
done | p50

parent=$($B sandbox create "$proj" --image python:3.13-slim 2>/dev/null)
printf 'fork to child exec    '
for _ in $(seq "$N"); do
  t=$(now); id=$($B sandbox fork "$parent" 2>/dev/null)
  $B sandbox exec "$id" -- true; echo "$(now) - $t" | bc
  echo "$id" >>"$proj/forks"
done | p50

printf 'rm                    '
for id in $(cat "$proj/forks"); do
  t=$(now); $B sandbox rm "$id" >/dev/null; echo "$(now) - $t" | bc
done | p50
$B sandbox rm "$parent" >/dev/null

printf '%s at once           ' "$CONC"
t=$(now)
for i in $(seq "$CONC"); do
  ($B sandbox create "$proj" --name "bench$i" --memory 1 --cpus 1 --image python:3.13-slim >/dev/null 2>&1 || echo "bench$i failed" >&2) &
done
wait
echo "all ready in $(echo "$(now) - $t" | bc)s"
total=0; n=0
for pid in $(pgrep -f Virtualization.VirtualMachine); do
  f=$(footprint "$pid" 2>/dev/null | awk '/Footprint/ {print $(NF-5); exit}')
  [ -n "$f" ] && [ "$f" -lt 2000 ] && { total=$((total + f)); n=$((n + 1)); }
done
echo "idle footprint        $((total / (n > 0 ? n : 1))) MB per VM (n=$n, VMs under 2 GB)"
for i in $(seq "$CONC"); do echo "bench$i"; done | xargs $B sandbox rm >/dev/null
