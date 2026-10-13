#!/usr/bin/env bash
# Fleet and app hosting checks (docs/platform.md, tracks 2-3): a controller, two nodes and
# an edge on this Mac, each in its own REKCOD_HOME. Leaves the main VM alone.
#
#   REKCOD_E2E_DB=postgres://... scripts/e2e-fleet.sh
#
# The database is WIPED (schema public is dropped). Needs psql and git.
set -uo pipefail
R=${REKCOD:-$(cd "$(dirname "$0")/.." && pwd)/target/release/rekcod}
DB=${REKCOD_E2E_DB:?set REKCOD_E2E_DB to a Postgres URL this script may wipe}
failures=0
ok() { printf '  ok    %s\n' "$1"; }
bad() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
check() {
  local name=$1 out; shift
  if out=$("$@" 2>&1); then ok "$name"; else bad "$name"; printf '%s\n' "$out" | tail -4 | sed 's/^/        /'; fi
}

# Short homes: sandbox docker sockets must fit in sun_path.
C=$HOME/.rkfc N1=$HOME/.rkf1 N2=$HOME/.rkf2
pids=()
cleanup() {
  if [[ $failures -gt 0 ]]; then
    logs=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-e2e-fleet-logs-XXXXXX")
    cp "$C"/*.log "$logs/" 2>/dev/null; cp "$N1/node.log" "$logs/node1.log" 2>/dev/null; cp "$N2/node.log" "$logs/node2.log" 2>/dev/null
    echo "logs: $logs"
  fi
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
  for h in "$N1" "$N2"; do REKCOD_HOME=$h "$R" sandbox rm --all >/dev/null 2>&1; done
  rm -rf "$C" "$N1" "$N2"
}
trap cleanup EXIT
mkdir -p "$C" "$N1" "$N2"
export REKCOD_HOME=$C
for h in "$N1" "$N2"; do
  [[ -d $HOME/.rekcod/golden/default ]] && mkdir -p "$h/golden" && cp -cR "$HOME/.rekcod/golden/default" "$h/golden/"
done
psql -q "$DB" -c 'drop schema public cascade; create schema public' >/dev/null

# A repo the nodes clone: Python app, no Dockerfile (rekcod.toml's start command).
repo=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-fleet-XXXXXX")
git init -q --bare "$repo/app.git"
git clone -q "$repo/app.git" "$repo/w" 2>/dev/null
cat >"$repo/w/app.py" <<'EOF'
import http.server, os
V = open("VERSION").read().strip()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(s):
        b = f"{V} {os.environ.get('GREETING', '')}\n".encode()
        s.send_response(200); s.send_header("Content-Length", str(len(b))); s.end_headers(); s.wfile.write(b)
    def log_message(s, *a): print(*a, flush=True)
http.server.ThreadingHTTPServer(("0.0.0.0", 8000), H).serve_forever()
EOF
printf '[app]\nport = 8000\nstart = "python3 app.py"\n' >"$repo/w/rekcod.toml"
echo 3.13 >"$repo/w/.python-version"
commit() { (cd "$repo/w" && echo "$1" >VERSION && git add -A && git commit -qm "$1" && git push -q origin HEAD:main 2>/dev/null && git rev-parse HEAD); }
v1=$(commit v1)
url="$repo/app.git"

echo "fleet"
REKCOD_HOME=$C "$R" controller serve --db "$DB" --listen 127.0.0.1:17700 --domain apps.test --drain 2s >"$C/controller.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [[ -f $C/controller/admin-token ]] && break; sleep 0.2; done
join=$(cat "$C/controller/join-token") admin=$(cat "$C/controller/admin-token")
REKCOD_HOME=$N1 "$R" node join http://127.0.0.1:17700 "$join" --listen 127.0.0.1:17701 >"$N1/node.log" 2>&1 &
pids+=($!)
REKCOD_HOME=$N2 "$R" node join http://127.0.0.1:17700 "$join" --listen 127.0.0.1:17702 >"$N2/node.log" 2>&1 &
pids+=($!) node2=$!
sleep 2
cr() { REKCOD_HOME=$C "$R" "$@"; }
cr remote add admin http://127.0.0.1:17700 --token "$admin"
alice=$(cr controller token alice --max-sandboxes 2 --remote admin | sed -n 's/.*"token": "\(.*\)".*/\1/p')
bob=$(cr controller token bob --remote admin | sed -n 's/.*"token": "\(.*\)".*/\1/p')
cr remote add alice http://127.0.0.1:17700 --token "$alice"
cr remote add bob http://127.0.0.1:17700 --token "$bob"
check "two nodes ready" sh -c 'test "$("$0" node ls --remote admin | grep -c "\"ready\"")" = 2' "$R"

s1=$(cr sandbox create --remote alice --git "$url" --ref main --image alpine:3.22 2>"$C/err")
check "remote create from git" test -n "$s1"
check "remote exec sees the clone" sh -c 'test "$("$0" sandbox exec --remote alice "$1" -- cat VERSION)" = v1' "$R" "$s1"
check "remote exec keeps the exit code" sh -c '"$0" sandbox exec --remote alice "$1" -- sh -c "exit 5"; test $? = 5' "$R" "$s1"
check "another user can't reach it" sh -c '! "$0" sandbox exec --remote bob "$1" -- true' "$R" "$s1"
check "apply isn't offered remotely" sh -c '! "$0" sandbox apply --remote alice "$1"' "$R" "$s1"
first=$(cr sandbox ls --remote alice | sed -n 's/.*"node": "\(.*\)".*/\1/p' | head -1)
cr node drain "$first" --remote admin >/dev/null
s2=$(cr sandbox create --remote alice --git "$url" --ref main --image alpine:3.22 2>/dev/null)
check "drained node gets no new sandboxes" sh -c '"$0" sandbox ls --remote alice | grep -A1 "\"$1\"" | grep "\"node\"" | grep -vq "$2" || "$0" sandbox ls --remote alice | grep -B3 "\"$1\"" | grep "\"node\"" | grep -vq "$2"' "$R" "$s2" "$first"
check "quota (2) refuses a third" sh -c '! "$0" sandbox create --remote alice --git "$1" --ref main --image alpine:3.22' "$R" "$url"
check "remote rm" sh -c '"$0" sandbox rm --remote alice "$1" "$2"' "$R" "$s1" "$s2"
psql -q "$DB" -c "update nodes set state = 'ready'" >/dev/null

echo "apps"
REKCOD_HOME=$C "$R" edge serve --remote admin --listen 127.0.0.1:18088 --public-listen 127.0.0.1:18089 >"$C/edge.log" 2>&1 &
pids+=($!)
get() { curl -s -m 3 -H "Host: $1.apps.test" http://127.0.0.1:18088/; }
check "deploy (generated Dockerfile)" cr app deploy --remote admin --git "$url" --ref main --name hello --instances 2
sleep 2.5
check "edge serves the app" sh -c 'test "$(curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18088/)" = "v1 "'
check "new apps aren't on the public listener" sh -c 'curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18089/ | grep -q "no app"'
check "app public" cr app public --remote admin hello
sleep 2.5
check "public app on the public listener" sh -c 'test "$(curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18089/)" = "v1 "'
v2=$(commit v2)
: >"$C/codes"
( for _ in $(seq 400); do curl -s -o /dev/null -w '%{http_code}\n' -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18088/ >>"$C/codes"; sleep 0.05; done ) &
loop=$!
check "second deploy" cr app deploy --remote admin --git "$url" --ref main --name hello --instances 2
sleep 3
kill $loop 2>/dev/null; wait $loop 2>/dev/null
check "zero downtime (no non-200 during the deploy)" sh -c '! grep -qv "^200$" "$0" && test -s "$0"' "$C/codes"
check "new release live" sh -c 'test "$(curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18088/)" = "v2 "'
(cd "$repo/w" && git rm -q VERSION && git commit -qm broken && git push -q origin HEAD:main 2>/dev/null)
check "broken release fails" sh -c '! "$0" app deploy --remote admin --git "$1" --ref main --name hello' "$R" "$url"
check "old release keeps serving" sh -c 'test "$(curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18088/)" = "v2 "'
check "rollback" cr app rollback --remote admin hello
sleep 2.5
check "rolled back to v1" sh -c 'test "$(curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18088/)" = "v1 "'
check "env set redeploys" cr app env set --remote admin hello GREETING=hi
sleep 2.5
check "env reaches the app" sh -c 'test "$(curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18088/)" = "v1 hi"'
check "secret value isn't shown" sh -c '"$0" app secret set --remote admin hello TOKEN=s3cr3t && ! "$0" app status --remote admin hello | grep -q s3cr3t' "$R"
check "logs" sh -c '"$0" app logs --remote admin hello | grep -q GET' "$R"

body=$(printf '{"action":"opened","number":7,"repository":{"clone_url":"%s","html_url":"x"},"pull_request":{"head":{"sha":"%s","repo":{"clone_url":"%s"}}}}' "$url" "$v2" "$url")
ws=$(cat "$C/controller/webhook-secret")
sig="sha256=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$ws" | awk '{print $NF}')"
hook() { curl -s -m 120 -X POST -H "X-GitHub-Event: pull_request" -H "X-Hub-Signature-256: $2" -d "$1" http://127.0.0.1:17700/webhooks/github; }
check "webhook with a bad signature is refused" sh -c '! curl -sf -m 5 -X POST -H "X-GitHub-Event: pull_request" -H "X-Hub-Signature-256: sha256=00" -d "{}" http://127.0.0.1:17700/webhooks/github'
hook "$body" "$sig" >>"$C/hook.log"; echo >>"$C/hook.log"
sleep 2.5
check "PR preview deployed (inherits env)" sh -c 'test "$(curl -s -m 3 -H "Host: hello-pr-7.apps.test" http://127.0.0.1:18088/)" = "v2 hi"'
check "PR previews stay private" sh -c 'curl -s -m 3 -H "Host: hello-pr-7.apps.test" http://127.0.0.1:18089/ | grep -q "no app"'
check "app private" sh -c '"$0" app private --remote admin hello && sleep 2.5 && curl -s -m 3 -H "Host: hello.apps.test" http://127.0.0.1:18089/ | grep -q "no app"' "$R"
body=${body/opened/closed}
sig="sha256=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$ws" | awk '{print $NF}')"
hook "$body" "$sig" >>"$C/hook.log"; echo >>"$C/hook.log"
sleep 2.5
check "PR preview removed on close" sh -c 'curl -s -m 3 -H "Host: hello-pr-7.apps.test" http://127.0.0.1:18088/ | grep -q "no app"'
check "destroy" cr app destroy --remote admin hello

kill "$node2"
sleep 22
check "silent node marked lost" sh -c '"$0" node ls --remote admin | grep -q "\"lost\""' "$R"

echo
if [[ $failures -eq 0 ]]; then echo "all fleet checks passed"; else echo "$failures fleet check(s) failed"; exit 1; fi
