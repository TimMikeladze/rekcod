#!/usr/bin/env bash
# First access to a bind-mounted tree right after `rekcod start`, with and
# without the boot-time warming of recently used bind sources (docs/phase5.md §1).
#
#   bench/cold_start.sh            # needs rekcod built (target/release/rekcod)
set -euo pipefail
cd "$(dirname "$0")/.."
R="$PWD/target/release/rekcod"
tree="$HOME/rekcod-coldbench"
if [[ ! -d $tree ]]; then
  python3 - "$tree" <<'PY'
import os, sys
root = sys.argv[1]
for p in range(500):
    for d in range(3):
        dd = os.path.join(root, f"pkg{p}", *[f"d{i}" for i in range(d + 1)])
        os.makedirs(dd, exist_ok=True)
        for f in range(13):
            open(os.path.join(dd, f"f{f}.js"), "w").write("module.exports = 1;\n" * 25)
PY
fi
walk() { docker run --rm -v "$tree:/t:ro" python:3.13-slim python3 -c 'import os,time; t=time.perf_counter(); [os.lstat(os.path.join(d,f)) for d,_,fs in os.walk("/t") for f in fs]; print(round((time.perf_counter()-t)*1000))'; }
restart() { "$R" stop --no-switch >/dev/null; "$R" start --no-switch >/dev/null; }

docker pull -q python:3.13-slim >/dev/null
docker run --rm -v "$tree:/t:ro" alpine:3.22 true  # remember it as a bind source
restart; sleep 3
echo "warmed at boot (walk 3 s after start): $(walk) ms"
rm -f "$HOME/.rekcod/vm/warm-paths"
restart; sleep 3
echo "not warmed (walk 3 s after start):     $(walk) ms"
echo "warm repeat:                           $(walk) ms"
