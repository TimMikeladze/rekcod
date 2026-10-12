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
# Start from another context so the switch and the restore are both tested.
if [[ $prev == rekcod ]]; then docker context use default >/dev/null; switched_back=1; prev=default; fi
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

echo "switcher gaps"
docker run -d --rm --name rekcod-e2e-udp -p 18053:5353/udp alpine:3.22 sh -c 'nc -lu -p 5353 -e cat' >/dev/null
sleep 1.5
check "published UDP port echoes" sh -c 'test "$(echo ping | nc -u -w2 127.0.0.1 18053)" = ping'
docker rm -f rekcod-e2e-udp >/dev/null 2>&1
python3 -m http.server 18077 --bind 127.0.0.1 --directory tests/e2e/stack/www >/dev/null 2>&1 & srv=$!
sleep 0.5
check "host.docker.internal reaches Mac 127.0.0.1" sh -c 'docker run --rm alpine:3.22 wget -qO- http://host.docker.internal:18077/ | grep -q "rekcod e2e"'
kill $srv 2>/dev/null
tmpf=$(mktemp /tmp/rekcod-e2e.XXXX); echo mac-tmp >"$tmpf"
check "Mac /tmp bind mount" sh -c "test \"\$(docker run --rm -v $tmpf:/f alpine:3.22 cat /f)\" = mac-tmp"
rm -f "$tmpf"
check "rekcod compact" "$R" compact

echo "docker passthrough (docs/cli-passthrough.md)"
check "rekcod run keeps docker's exit code" sh -c '"$0" run --rm alpine:3.22 sh -c "exit 7"; test $? = 7' "$R"
check "rekcod compose version" "$R" compose version
"$R" run -d --name rekcod-e2e-pt alpine:3.22 sleep 60 >/dev/null
check "rekcod stop <container> is docker's" sh -c '"$0" stop -t 1 rekcod-e2e-pt && "$0" status | grep -q "rekcod running"' "$R"
docker container rm -f rekcod-e2e-pt >/dev/null 2>&1

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
# APFS is case-insensitive: cached negatives/aliases for other spellings must go.
docker exec rekcod-e2e-coh sh -c 'cat /c/case1 /c/case2; cat /c/CASE3' >/dev/null 2>&1
echo mac >"$coh/Case1"
check "Mac create Case1 visible as case1" see 'test "$(cat /c/case1)" = mac'
check "guest create Case2 visible as case2" docker exec rekcod-e2e-coh sh -c 'echo guest > /c/Case2 && test "$(cat /c/case2)" = guest'
docker exec rekcod-e2e-coh cat /c/CASE1 >/dev/null 2>&1
rm "$coh/Case1"
check "Mac delete Case1 drops alias CASE1" see '! test -e /c/CASE1'
echo x >"$coh/case3" && mv "$coh/case3" "$coh/Case3"
check "Mac case-only rename visible" see 'ls /c | grep -qx Case3 && test "$(cat /c/CASE3)" = x'
docker rm -f rekcod-e2e-coh >/dev/null 2>&1

echo "phase 5 (docs/phase5.md)"
check "status --json" sh -c '"$0" status --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d[\"running\"] and d[\"disk\"][\"size\"] > 0"' "$R"
lw="$work/local"; mkdir -p "$lw"
check "dev.rekcod.local puts node_modules on a VM volume" sh -c "docker run --rm --name rekcod-e2e-lbl -l dev.rekcod.local=node_modules -w /app -v '$lw:/app' alpine:3.22 sh -c 'echo x > node_modules/f && mount | grep -q \" /app/node_modules type ext4\"' && test ! -e '$lw/node_modules/f'"
docker volume rm -f rekcod-local_rekcod-e2e-lbl_app-node_modules >/dev/null 2>&1
sp="$work/snap"; mkdir -p "$sp"
printf 'services:\n  a:\n    image: alpine:3.22\n    command: sleep 600\n    init: true\n    volumes: [data:/d]\nvolumes:\n  data:\n' >"$sp/compose.yaml"
(cd "$sp" && docker compose -p rekcod-e2e-snap up -d >/dev/null 2>&1 && docker compose -p rekcod-e2e-snap exec -T a sh -c 'echo v1 > /d/f')
check "snap save, change, restore" sh -c "cd '$sp' && '$R' snap save -p rekcod-e2e-snap s1 && docker compose -p rekcod-e2e-snap exec -T a sh -c 'echo v2 > /d/f' && '$R' snap restore -p rekcod-e2e-snap s1 && test \"\$(docker compose -p rekcod-e2e-snap exec -T a cat /d/f)\" = v1"
check "snap ls --json" sh -c "cd '$sp' && '$R' snap ls -p rekcod-e2e-snap --json | grep -q '\"name\": \"s1\"'"
(cd "$sp" && "$R" snap rm -p rekcod-e2e-snap s1 >/dev/null 2>&1; docker compose -p rekcod-e2e-snap down -v -t 1 >/dev/null 2>&1)
xp="$work/x"; mkdir -p "$xp"
check "rekcod x keeps the exit code" sh -c "cd '$xp' && '$R' x --image alpine:3.22 sh -c 'exit 3'; test \$? = 3"
(cd "$xp" && "$R" x --image python:3.13-slim python3 -m http.server 18093 --bind 127.0.0.1 >/dev/null 2>&1 &)
check "rekcod x forwards a 127.0.0.1 listener to the Mac" sh -c 'for i in $(seq 1 60); do curl -sf -o /dev/null localhost:18093 && exit 0; sleep 1; done; exit 1'
docker rm -f $(docker ps -q --filter label=dev.rekcod.x) >/dev/null 2>&1
echo inside >"$xp/f.txt"
check "x --sandbox sees the project, not the rest of \$HOME" sh -c "cd '$xp' && test \"\$('$R' x --sandbox --image alpine:3.22 sh -c 'cat f.txt; ls -A /Users/*/ | wc -l' | tr '\n' ' ')\" = 'inside 1 '"
check "x --sandbox can't reach the Mac" sh -c "cd '$xp' && '$R' x --sandbox --image alpine:3.22 sh -c '! wget -qO- -T 3 http://host.docker.internal:22/'"

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
[[ -n ${switched_back:-} ]] && docker context use rekcod >/dev/null

echo
if [[ $failures -eq 0 ]]; then echo "all checks passed"; else echo "$failures check(s) failed"; exit 1; fi
