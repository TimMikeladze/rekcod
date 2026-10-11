#!/usr/bin/env bash
# FSEvents overflow: a burst big enough that macOS coalesces/drops events
# (MustScanSubDirs), while files the guest has cached are rewritten mid-burst.
# The daemon must turn the overflow into a subtree invalidation: afterwards
# every cached file reads back its new content in the container.
#
#   tests/stress/overflow.sh [files=200000]
#
# A real drop needs more than FSEvents' queue holds (200k files in 12 s on an
# M5 Pro didn't drop). To exercise the subtree path deterministically, start
# the daemon with the test hook and point the script at the same directory:
#   REKCOD_TEST_FORCE_RESCAN=$HOME/.rekcod-overflow-forced rekcod start
#   OVERFLOW_ROOT=$HOME/.rekcod-overflow-forced tests/stress/overflow.sh 20000
set -uo pipefail
n=${1:-200000}
ctx=${CONTEXT:-rekcod}
d=(docker --context "$ctx")
root=${OVERFLOW_ROOT:-$(mktemp -d "$HOME/.rekcod-overflow.XXXX")}
mkdir -p "$root"
trap '"${d[@]}" rm -f rekcod-overflow >/dev/null 2>&1; rm -rf "$root"' EXIT
log="$HOME/.rekcod/logs/daemon.log"
before=$(grep -c "invalidating everything under" "$log" 2>/dev/null || echo 0)

mkdir -p "$root/hot"
for i in $(seq 1 200); do echo "v1-$i" >"$root/hot/f$i"; done
"${d[@]}" run -d --name rekcod-overflow -v "$root:/c" alpine:3.22 sleep 3600 >/dev/null
"${d[@]}" exec rekcod-overflow sh -c 'cat /c/hot/* >/dev/null; ls /c/hot/g1 2>/dev/null' # warm + cached negative

echo "burst: $n files"
t0=$(date +%s)
python3 - "$root" "$n" <<'EOF'
import os, sys
root, n = sys.argv[1], int(sys.argv[2])
for i in range(n):
    d = os.path.join(root, "burst", str(i // 1000))
    if i % 1000 == 0:
        os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, f"{i}.txt"), "w") as f:
        f.write("x")
    if i == n // 2:  # rewrite the cached files mid-burst
        for j in range(1, 201):
            with open(os.path.join(root, "hot", f"f{j}"), "w") as f:
                f.write(f"v2-{j}\n")
        open(os.path.join(root, "hot", "g1"), "w").close()
EOF
echo "burst done in $(( $(date +%s) - t0 ))s"

fail=0
ok=0
for i in $(seq 1 30); do
  bad=$("${d[@]}" exec rekcod-overflow sh -c 'n=0; for j in $(seq 1 200); do [ "$(cat /c/hot/f$j)" = "v2-$j" ] || n=$((n+1)); done; [ -e /c/hot/g1 ] || n=$((n+1)); echo $n')
  [ "$bad" = 0 ] && { ok=1; break; }
  sleep 1
done
after=$(grep -c "invalidating everything under" "$log" 2>/dev/null || echo 0)
echo "subtree invalidations during burst: $((after - before))"
if [ "$ok" = 1 ]; then echo "ok: all 201 cached entries current (after ${i}s of polling)"; else echo "FAIL: $bad of 201 cached entries still stale after 30s"; fail=1; fi
cnt=$("${d[@]}" exec rekcod-overflow sh -c 'find /c/burst -type f | wc -l')
[ "$cnt" = "$n" ] && echo "ok: container sees all $n burst files" || { echo "FAIL: container sees $cnt of $n burst files"; fail=1; }
exit $fail
