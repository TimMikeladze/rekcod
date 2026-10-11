#!/usr/bin/env bash
# End-to-end check of the switch-over flow (docs/mvp.md "Done when").
# Uses the real docker CLI and compose against rekcod; restores your context.
#
#   scripts/e2e.sh                       # core checks
#   E2E_IMPORT_FROM=orbstack scripts/e2e.sh   # also test import from that context
set -uo pipefail
cd "$(dirname "$0")/.."
R="$PWD/target/release/rekcod"
failures=0
ok() { printf '  ok    %s\n' "$1"; }
bad() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
check() { local name=$1; shift; if "$@" >/dev/null 2>&1; then ok "$name"; else bad "$name"; fi; }

unset DOCKER_HOST DOCKER_CONTEXT
prev=$(docker context show)
work=$(mktemp -d "$HOME/.rekcod-e2e.XXXX")
trap 'docker compose -p rekcod-e2e -f "$work/compose.yaml" down -v >/dev/null 2>&1; rm -rf "$work"' EXIT

echo "start"
"$R" start || { echo "rekcod start failed"; exit 1; }
check "context switched to rekcod" test "$(docker context show)" = rekcod

echo "engine"
check "hello-world" sh -c 'docker run --rm hello-world | grep -q "Hello from Docker"'
check "stdin half-close (docker run -i)" sh -c 'test "$(echo hi | docker run -i --rm alpine:3.22 cat)" = hi'
check "amd64 via Rosetta" sh -c 'test "$(docker run --rm --platform linux/amd64 alpine:3.22 uname -m)" = x86_64'
check "host.docker.internal resolves" docker run --rm alpine:3.22 getent hosts host.docker.internal
check "external DNS" docker run --rm alpine:3.22 nslookup example.com

echo "compose"
cp -R tests/e2e/stack/. "$work/"
compose="docker compose -p rekcod-e2e -f $work/compose.yaml"
check "up --build --wait (healthcheck-gated)" $compose up -d --build --wait
check "bind mount served on localhost:18080" sh -c 'curl -sf localhost:18080 | grep -q "rekcod e2e"'
check "built image served on localhost:18081" sh -c 'curl -sf localhost:18081 | grep -q "built by rekcod"'
echo "<h1>edited</h1>" >"$work/www/index.html"
# Mac-side changes reach the guest's caches via FSEvents (~25 ms): poll briefly.
check "Mac-side edit visible in container" sh -c 'for i in 1 2 3 4 5 6 7 8 9 10; do curl -sf localhost:18080 | grep -q edited && exit 0; sleep 0.1; done; exit 1'
docker run -d --name rekcod-e2e-watch -v "$work/www:/w" alpine:3.22 inotifyd - /w >/dev/null
sleep 1
echo "<h1>hot reload</h1>" >"$work/www/index.html"
check "Mac-side edit fires inotify in container" sh -c 'for i in 1 2 3 4 5 6 7 8 9 10; do docker logs rekcod-e2e-watch 2>&1 | grep -q index.html && exit 0; sleep 0.2; done; exit 1'
printf 'atomic' >"$work/www/.tmp-save" && mv "$work/www/.tmp-save" "$work/www/index.html"
check "atomic (rename) save readable in container" sh -c "for i in 1 2 3 4 5 6 7 8 9 10; do docker exec rekcod-e2e-watch cat /w/index.html 2>/dev/null | grep -q atomic && exit 0; sleep 0.2; done; exit 1"
echo doomed >"$work/www/doomed.txt"
docker exec rekcod-e2e-watch cat /w/doomed.txt >/dev/null 2>&1
rm "$work/www/doomed.txt"
sleep 1
check "Mac-side delete stays deleted" test ! -e "$work/www/doomed.txt"
docker rm -f rekcod-e2e-watch >/dev/null 2>&1
check "second up recreates nothing" sh -c "out=\$($compose up -d 2>&1) && ! echo \"\$out\" | grep -qE 'Recreat|Creat'"
check "status lists forwarded port" sh -c "'$R' status | grep -q 18080"
$compose down -v >/dev/null 2>&1
check "port released after down" sh -c '! curl -sf --max-time 2 localhost:18080'

echo "coherence (Mac-side changes vs cached guest view)"
coh="$work/coh"; mkdir -p "$coh"
docker run -d --name rekcod-e2e-coh -v "$coh:/c" alpine:3.22 sleep 600 >/dev/null
# Poll the container's view for up to 2s; caches are warmed by the previous step.
see() { local i; for i in $(seq 1 20); do docker exec rekcod-e2e-coh sh -c "$1" >/dev/null 2>&1 && return 0; sleep 0.1; done; return 1; }
echo one >"$coh/a"; see 'test "$(cat /c/a)" = one'; docker exec rekcod-e2e-coh sh -c 'ls /c; stat /c/new' >/dev/null 2>&1
echo two >"$coh/a"
check "modify (same size) visible" see 'test "$(cat /c/a)" = two'
echo hi >"$coh/new"
check "create over cached negative lookup" see 'test "$(cat /c/new)" = hi'
check "new name appears in cached listing" see 'ls /c | grep -qx new'
printf three >"$coh/.t" && mv "$coh/.t" "$coh/a"
check "rename-over (atomic save) visible" see 'test "$(cat /c/a)" = three'
rm "$coh/a"
check "delete visible" see '! test -e /c/a'
mkdir "$coh/d" && echo x >"$coh/d/f"
check "mkdir + write visible" see 'test "$(cat /c/d/f)" = x'
chmod 600 "$coh/new"
check "chmod visible" see 'test "$(stat -c %a /c/new)" = 600'
docker exec rekcod-e2e-coh sh -c 'echo from-guest > /c/g'
check "guest write visible on the Mac" test "$(cat "$coh/g" 2>/dev/null)" = from-guest
docker rm -f rekcod-e2e-coh >/dev/null 2>&1

if [[ -n "${E2E_IMPORT_FROM:-}" ]]; then
  echo "import from $E2E_IMPORT_FROM"
  src=(docker --context "$E2E_IMPORT_FROM")
  "${src[@]}" volume rm -f rekcod-e2e-vol >/dev/null 2>&1
  docker volume rm -f rekcod-e2e-vol >/dev/null 2>&1
  "${src[@]}" volume create --label e2e=yes rekcod-e2e-vol >/dev/null
  "${src[@]}" run --rm -v rekcod-e2e-vol:/v alpine:3.22 sh -c 'echo imported-data > /v/file' >/dev/null
  check "import volume + image" "$R" import --from "$E2E_IMPORT_FROM" rekcod-e2e-vol alpine:3.22
  check "volume data arrived" sh -c 'test "$(docker run --rm -v rekcod-e2e-vol:/v alpine:3.22 cat /v/file)" = imported-data'
  check "volume label kept" sh -c 'test "$(docker volume inspect -f "{{.Labels.e2e}}" rekcod-e2e-vol)" = yes'
  "${src[@]}" volume rm -f rekcod-e2e-vol >/dev/null 2>&1
  docker volume rm -f rekcod-e2e-vol >/dev/null 2>&1
fi

echo "stop"
"$R" stop >/dev/null
check "context restored to $prev" test "$(docker context show)" = "$prev"

echo
if [[ $failures -eq 0 ]]; then echo "all checks passed"; else echo "$failures check(s) failed"; exit 1; fi
