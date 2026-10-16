#!/usr/bin/env bash
# A throwaway fleet on this Mac for working on the dashboard's fleet views: a controller,
# two nodes and an edge, each in its own REKCOD_HOME (the main VM is left alone), with an
# app that has a failed release, finished and failed jobs, and a schedule. Stays up until
# Ctrl-C, then removes everything it made.
#
#   scripts/dev-fleet.sh
#   # in another terminal, as it prints:
#   cd dashboard && REKCOD_HOME=~/.rkdc REKCOD_BIN=../target/release/rekcod npm run dev
set -uo pipefail
R=${REKCOD:-$(cd "$(dirname "$0")/.." && pwd)/target/release/rekcod}
# Short homes: sandbox docker sockets must fit in sun_path.
C=$HOME/.rkdc N1=$HOME/.rkd1 N2=$HOME/.rkd2
CP=${CONTROLLER_PORT:-17800} EP=${EDGE_PORT:-18188}
# The fleet runs from its own copy, so rebuilding target/release (which re-signs the file in
# place, and macOS kills processes whose binary changes) doesn't take it down. Nodes boot VMs,
# which needs the entitlement `cargo build` leaves off.
# Under target/ so the binary finds ../../out/guest like the original does.
bin=$(cd "$(dirname "$0")/.." && pwd)/target/dev-fleet
mkdir -p "$bin"
cp "$R" "$bin/rekcod" && codesign --force --sign - --entitlements "$(dirname "$0")/../rekcod.entitlements" "$bin/rekcod" || exit 1
R=$bin/rekcod
pids=()
cleanup() {
  echo "dev-fleet: stopping"
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
  for h in "$N1" "$N2"; do REKCOD_HOME=$h "$R" sandbox rm --all >/dev/null 2>&1; done
  rm -rf "$C" "$N1" "$N2" "$repo" "$bin"
}
trap cleanup EXIT
trap 'exit 0' INT TERM
for h in "$C" "$N1" "$N2"; do
  [[ -e $h ]] && { echo "dev-fleet: $h exists (another dev-fleet running?)"; exit 1; }
done
mkdir -p "$C" "$N1" "$N2"
for h in "$N1" "$N2"; do
  [[ -d $HOME/.rekcod/golden/default ]] && mkdir -p "$h/golden" && cp -cR "$HOME/.rekcod/golden/default" "$h/golden/"
done

repo=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-devfleet-XXXXXX")
git init -q --bare "$repo/app.git"
git clone -q "$repo/app.git" "$repo/w" 2>/dev/null
cat >"$repo/w/app.py" <<'EOF'
import http.server, os
V = open("VERSION").read().strip()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(s):
        code = 500 if s.path == "/boom" else 200
        b = f"{V} {os.environ.get('GREETING', '')}\n".encode()
        s.send_response(code); s.send_header("Content-Length", str(len(b))); s.end_headers(); s.wfile.write(b)
    def log_message(s, *a): print(*a, flush=True)
http.server.ThreadingHTTPServer(("0.0.0.0", 8000), H).serve_forever()
EOF
printf '[app]\nport = 8000\nstart = "python3 app.py"\n' >"$repo/w/rekcod.toml"
echo 3.13 >"$repo/w/.python-version"
commit() { (cd "$repo/w" && echo "$1" >VERSION && git add -A && git commit -qm "$1" && git push -q origin HEAD:main 2>/dev/null && git rev-parse HEAD); }
v1=$(commit v1)
url="$repo/app.git"
cr() { REKCOD_HOME=$C "$R" "$@"; }

REKCOD_HOME=$C "$R" controller serve --http --auto-approve --listen 127.0.0.1:$CP --domain apps.test --drain 2s >"$C/controller.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [[ -f $C/controller/admin-token ]] && break; sleep 0.2; done
join=$(cat "$C/controller/join-token") admin=$(cat "$C/controller/admin-token")
REKCOD_HOME=$N1 "$R" node join http://127.0.0.1:$CP "$join" --listen 127.0.0.1:$((CP + 1)) >"$N1/node.log" 2>&1 &
pids+=($!)
REKCOD_HOME=$N2 "$R" node join http://127.0.0.1:$CP "$join" --listen 127.0.0.1:$((CP + 2)) >"$N2/node.log" 2>&1 &
pids+=($!)
sleep 2
cr remote add admin http://127.0.0.1:$CP --token "$admin"
REKCOD_HOME=$C "$R" edge serve --remote admin --listen 127.0.0.1:$EP >"$C/edge.log" 2>&1 &
pids+=($!)

echo "dev-fleet: deploying hello (2 instances), then v2, then a broken v3"
cr app deploy --git "$url" --ref main --name hello --instances 2 >/dev/null || echo "dev-fleet: first deploy failed"
commit v2 >/dev/null
cr app deploy --git "$url" --ref main --name hello --instances 2 >/dev/null || echo "dev-fleet: second deploy failed"
(cd "$repo/w" && git rm -q VERSION && git commit -qm broken && git push -q origin HEAD:main 2>/dev/null)
cr app deploy --git "$url" --ref main --name hello >/dev/null 2>&1 && echo "dev-fleet: the broken deploy didn't fail?"
cr app deploy --git "$url" --ref "$v1" --name nap --idle-stop 30s >/dev/null || echo "dev-fleet: nap deploy failed"

echo "dev-fleet: jobs"
job() { cr job run --git "$url" --ref "$v1" --image alpine:3.22 "$@"; }
job --name ok -- sh -c 'cat VERSION; echo done' >/dev/null 2>&1
job --name broken -- sh -c 'echo about to fail; exit 3' >/dev/null 2>&1
job --name slow --detach -- sh -c 'for i in $(seq 120); do echo tick $i; sleep 1; done' >/dev/null
job --name tick --every 60s -- sh -c 'date; echo tick' >/dev/null

# Some traffic for the per-app charts, with a few 500s.
( while :; do
    for _ in $(seq 20); do curl -s -o /dev/null -m 3 -H "Host: hello.apps.test" http://127.0.0.1:$EP/; done
    curl -s -o /dev/null -m 3 -H "Host: hello.apps.test" http://127.0.0.1:$EP/boom
    sleep 2
  done ) &
pids+=($!)

cat <<EOF

dev-fleet: up. Controller http://127.0.0.1:$CP, edge http://127.0.0.1:$EP (Host: hello.apps.test).
  REKCOD_HOME=$C $R app status hello
  cd dashboard && REKCOD_HOME=$C REKCOD_BIN=$R npm run dev
Ctrl-C stops it and removes $C $N1 $N2.
EOF
wait
