#!/usr/bin/env bash
# Fleet and app hosting checks (docs/platform.md, tracks 2-3): a controller, two nodes and
# an edge on this Mac, each in its own REKCOD_HOME. Leaves the main VM alone.
# Needs git and sqlite3 (both ship with macOS).
set -uo pipefail
R=${REKCOD:-$(cd "$(dirname "$0")/.." && pwd)/target/release/rekcod}
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
# The test repo is a local bare repo; real fleets only fetch over https/ssh/http.
export REKCOD_ALLOW_LOCAL_GIT=1
for h in "$N1" "$N2"; do
  [[ -d $HOME/.rekcod/golden/default ]] && mkdir -p "$h/golden" && cp -cR "$HOME/.rekcod/golden/default" "$h/golden/"
done

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
REKCOD_HOME=$C "$R" controller serve --listen 127.0.0.1:17700 --domain apps.test --drain 2s >"$C/controller.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [[ -f $C/controller/ca-fingerprint ]] && break; sleep 0.2; done
join=$(cat "$C/controller/join-token") admin=$(cat "$C/controller/admin-token") fp=$(cat "$C/controller/ca-fingerprint")
check "plain http to the https controller fails" sh -c '! curl -sf -m 3 http://127.0.0.1:17700/apps'
check "the CA's fingerprint is checked" sh -c '! REKCOD_HOME="$1" "$0" remote add bad https://127.0.0.1:17700 --token x --ca-fingerprint sha256:00' "$R" "$C"
REKCOD_HOME=$N1 "$R" node join https://127.0.0.1:17700 "$join" --listen 127.0.0.1:17701 --ca-fingerprint "$fp" >"$N1/node.log" 2>&1 &
pids+=($!)
REKCOD_HOME=$N2 "$R" node join https://127.0.0.1:17700 "$join" --listen 127.0.0.1:17702 --ca-fingerprint "$fp" >"$N2/node.log" 2>&1 &
pids+=($!) node2=$!
sleep 2
cr() { REKCOD_HOME=$C "$R" "$@"; }
cr remote add admin https://127.0.0.1:17700 --token "$admin" --ca-fingerprint "$fp"
alice=$(cr controller token alice --max-sandboxes 2 --remote admin | sed -n 's/.*"token": "\(.*\)".*/\1/p')
bob=$(cr controller token bob --remote admin | sed -n 's/.*"token": "\(.*\)".*/\1/p')
cr remote add alice https://127.0.0.1:17700 --token "$alice" --ca-fingerprint "$fp"
cr remote add bob https://127.0.0.1:17700 --token "$bob" --ca-fingerprint "$fp"
check "new nodes wait for approval" sh -c 'test "$("$0" node ls --remote admin | grep -c "\"pending\"")" = 2' "$R"
check "a pending node gets nothing" sh -c '! "$0" sandbox create --remote alice --git "$1" --ref main --image alpine:3.22' "$R" "$url"
for n in $(cr node ls --remote admin | sed -n 's/.*"id": "\(n-[a-z0-9]*\)".*/\1/p'); do cr node approve "$n" --remote admin >/dev/null; done
check "two nodes ready" sh -c 'test "$("$0" node ls --remote admin | grep -c "\"ready\"")" = 2' "$R"
check "a user token can't read routes" sh -c '! "$0" edge bans --remote alice' "$R"
edge=$(cr controller token edge --edge --remote admin | sed -n 's/.*"token": "\(.*\)".*/\1/p')
cr remote add edge https://127.0.0.1:17700 --token "$edge" --ca-fingerprint "$fp"
check "an edge token can't list nodes" sh -c '! "$0" node ls --remote edge' "$R"

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
sqlite3 "$C/controller/fleet.db" "update nodes set state = 'ready'"

echo "apps"
REKCOD_HOME=$C "$R" edge serve --remote edge --listen 127.0.0.1:18088 --public-listen 127.0.0.1:18089 >"$C/edge.log" 2>&1 &
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
hook() { curl -s -m 120 -X POST -H "X-GitHub-Event: pull_request" -H "X-Hub-Signature-256: $2" -d "$1" --cacert "$C/controller/ca.pem" https://127.0.0.1:17700/webhooks/github; }
check "webhook with a bad signature is refused" sh -c '! curl -sf -m 5 -X POST -H "X-GitHub-Event: pull_request" -H "X-Hub-Signature-256: sha256=00" -d "{}" --cacert "$C/controller/ca.pem" https://127.0.0.1:17700/webhooks/github'
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

echo "scale to zero, recycle"
check "deploy with --idle-stop 5s" cr app deploy --remote admin --git "$url" --ref "$v1" --name nap --idle-stop 5s
napget() { curl -s -m 90 -H "Host: nap.apps.test" http://127.0.0.1:18088/; }
sleep 3
check "awake app serves" sh -c 'test "$(curl -s -m 3 -H "Host: nap.apps.test" http://127.0.0.1:18088/)" = "v1 "'
asleep() { cr app ls --remote admin | grep -A8 '"name": "nap"' | grep -q '"sleeping": true'; }
for _ in $(seq 30); do asleep && break; sleep 1; done
check "idle app goes to sleep" asleep
vmstopped() { for h in "$N1" "$N2"; do REKCOD_HOME=$h "$R" sandbox ls; done | grep "nap-" | grep -q stopped; }
for _ in $(seq 40); do vmstopped && break; sleep 1; done
check "its VM is stopped" vmstopped
t0=$(date +%s)
check "a request wakes it (held, then 200)" sh -c 'test "$(curl -s -m 90 -H "Host: nap.apps.test" http://127.0.0.1:18088/)" = "v1 "'
echo "        woke in $(( $(date +%s) - t0 ))s"
check "awake again in app status" sh -c '"$0" app status --remote admin nap | grep -q "\"state\": \"live\""' "$R"
check "woke the same VMs (no redeploy)" sh -c '"$0" app status --remote admin nap | grep -q "\"live_release\": 1,"' "$R"
before=$(cr app status --remote admin nap | sed -n 's/.*"sandbox": "\(.*\)".*/\1/p' | sort | tr '\n' ' ')
: >"$C/codes"
( for _ in $(seq 200); do curl -s -o /dev/null -w '%{http_code}\n' -m 3 -H "Host: nap.apps.test" http://127.0.0.1:18088/ >>"$C/codes"; sleep 0.05; done ) &
loop=$!
check "app recycle" cr app recycle --remote admin nap
sleep 3
kill $loop 2>/dev/null; wait $loop 2>/dev/null
after=$(cr app status --remote admin nap | grep -B4 '"state": "live"' | sed -n 's/.*"sandbox": "\(.*\)".*/\1/p' | sort | tr '\n' ' ')
check "recycle gave new instances" test "$before" != "$after"
check "no non-200 during the recycle" sh -c '! grep -qv "^200$" "$0" && test -s "$0" || { sort "$0" | uniq -c; false; }' "$C/codes"
check "destroy nap" cr app destroy --remote admin nap

echo "jobs"
job() { cr job run --remote alice --git "$url" --ref "$v1" --image alpine:3.22 "$@"; }
out=$(job -- sh -c 'cat VERSION; echo hi; exit 3' 2>/dev/null); code=$?
check "job exit code comes back" test "$code" = 3
check "job output streams (sees the clone)" sh -c 'printf "%s" "$0" | grep -q hi && printf "%s" "$0" | grep -q v1' "$out"
job --timeout 2s -- sleep 30 >/dev/null 2>&1; code=$?
check "--timeout kills it with 124" test "$code" = 124
jid=$(job --detach -- sleep 120 2>/dev/null | sed -n 's/.*"id": "\(.*\)".*/\1/p')
check "detached job starts" test -n "$jid"
sleep 4
check "another user can't read it" sh -c '! "$0" job logs --remote bob "$1"' "$R" "$jid"
check "job cancel" cr job cancel --remote alice "$jid"
check "cancelled in job ls" sh -c '"$0" job ls --remote alice | grep -A8 "\"$1\"" | grep -q cancelled' "$R" "$jid"
check "schedule every 3s" job --name tick --every 3s -- echo tick
sleep 25
check "schedule ran at least twice" sh -c 'test "$("$0" job ls --remote alice | grep -c "\"schedule\": \"tick\"")" -ge 2' "$R"
check "unschedule" cr job unschedule --remote alice tick

echo "namespaces"
check "ns create (4 CPUs, 4 GiB)" cr ns create team --max-cpus 4 --max-memory 4 --remote admin
check "member add" cr ns member add team alice --remote admin
check "viewer add" cr ns member add team bob --role viewer --remote admin
carol=$(cr controller token carol --remote admin | sed -n 's/.*"token": "\(.*\)".*/\1/p')
cr remote add carol https://127.0.0.1:17700 --token "$carol" --ca-fingerprint "$fp"
check "members see it in ns ls" sh -c '"$0" ns ls --remote bob | grep -q "\"name\": \"team\""' "$R"
check "others don't" sh -c '! "$0" ns ls --remote carol | grep -q "\"team\""' "$R"
s3=$(cr sandbox create --remote alice --ns team --git "$url" --ref main --image alpine:3.22 --cpus 1 --memory 1 2>"$C/err")
check "sandbox in a namespace" test -n "$s3"
check "viewer lists it" sh -c '"$0" sandbox ls --remote bob --ns team | grep -q "$1"' "$R" "$s3"
check "viewer can't exec" sh -c '! "$0" sandbox exec --remote bob --ns team "$1" -- true' "$R" "$s3"
check "non-member can't reach it" sh -c '! "$0" sandbox exec --remote carol --ns team "$1" -- true' "$R" "$s3"
check "it isn't in default" sh -c '! "$0" sandbox exec --remote alice "$1" -- true' "$R" "$s3"
check "namespace quota refuses 4 more CPUs" sh -c '"$0" sandbox create --remote alice --ns team --git "$1" --ref main --image alpine:3.22 --cpus 4 --memory 1 2>&1 | grep -q "of 4 CPUs"' "$R" "$url"
cr sandbox rm --remote alice --ns team "$s3" >/dev/null
check "deploy into team" cr app deploy --remote alice --ns team --git "$url" --ref "$v1" --name hello
sleep 2.5
check "edge serves hello--team" sh -c 'test "$(curl -s -m 3 -H "Host: hello--team.apps.test" http://127.0.0.1:18088/)" = "v1 "'
check "viewer can't deploy" sh -c '! "$0" app deploy --remote bob --ns team --git "$1" --ref "$2" --name other' "$R" "$url" "$v1"
check "secret set" cr app secret set --remote alice --ns team hello GREETING=psst
sleep 2.5
check "sealed secret reaches the app" sh -c 'test "$(curl -s -m 3 -H "Host: hello--team.apps.test" http://127.0.0.1:18088/)" = "v1 psst"'
check "fleet.db has no plain secret" sh -c '! sqlite3 "$0" "select secrets from apps" | grep -q psst' "$C/controller/fleet.db"
check "ns rm refuses while it has apps" sh -c '! "$0" ns rm team --remote admin' "$R"
check "ns rm --force" cr ns rm team --force --remote admin
sleep 2.5
check "its app is gone from the edge" sh -c 'curl -s -m 3 -H "Host: hello--team.apps.test" http://127.0.0.1:18088/ | grep -q "no app"'

kill "$node2"
sleep 22
check "silent node marked lost" sh -c '"$0" node ls --remote admin | grep -q "\"lost\""' "$R"

echo
if [[ $failures -eq 0 ]]; then echo "all fleet checks passed"; else echo "$failures fleet check(s) failed"; exit 1; fi
