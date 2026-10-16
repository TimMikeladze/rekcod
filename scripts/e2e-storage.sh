#!/usr/bin/env bash
# RustFS checks (docs/rustfs.md): `storage serve`, a controller with --storage and one node,
# each in its own REKCOD_HOME. Leaves the main VM alone. The app is alpine + busybox nc, so
# the sandboxes don't need the internet. Needs git, curl and sqlite3 (all ship with macOS).
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
C=$HOME/.rksc N=$HOME/.rksn
pids=()
cleanup() {
  if [[ $failures -gt 0 ]]; then
    logs=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-e2e-storage-logs-XXXXXX")
    cp "$C"/*.log "$N"/*.log "$logs/" 2>/dev/null
    echo "logs: $logs"
  fi
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
  REKCOD_HOME=$N "$R" sandbox rm --all >/dev/null 2>&1
  # Keep the RustFS download for the next run.
  [[ -f $C/bin/rustfs-1.0.1 ]] && mkdir -p "$HOME/.rekcod/bin" && mv -n "$C/bin/rustfs-1.0.1" "$HOME/.rekcod/bin/" 2>/dev/null
  rm -rf "$C" "$N" "$repo"
}
trap cleanup EXIT
mkdir -p "$C/bin" "$N"
[[ -f $HOME/.rekcod/bin/rustfs-1.0.1 ]] && cp -c "$HOME/.rekcod/bin/rustfs-1.0.1" "$C/bin/"
[[ -d $HOME/.rekcod/golden/default ]] && mkdir -p "$N/golden" && cp -cR "$HOME/.rekcod/golden/default" "$N/golden/"
export REKCOD_HOME=$C

repo=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-storage-XXXXXX")
git init -q --bare "$repo/app.git"
git clone -q "$repo/app.git" "$repo/w" 2>/dev/null
# Answers every request with VERSION and its S3 env.
cat >"$repo/w/Dockerfile" <<'EOF'
FROM alpine:3.22
COPY VERSION /VERSION
CMD ["sh","-c","while true; do { printf 'HTTP/1.0 200 OK\\r\\n\\r\\n'; cat /VERSION; env | grep -E '^(S3_|AWS_)' | sort; } | nc -l -p 8000 >/dev/null; done"]
EOF
printf '[app]\nport = 8000\n\n[storage]\nbucket = true\n' >"$repo/w/rekcod.toml"
commit() { (cd "$repo/w" && echo "$1" >VERSION && git add -A && git commit -qm "$1" && git push -q origin HEAD:main 2>/dev/null && git rev-parse HEAD); }
v1=$(commit v1)
url="$repo/app.git"

echo "storage"
"$R" storage serve --listen 127.0.0.1:17710 >"$C/storage.log" 2>&1 &
pids+=($!)
for _ in $(seq 240); do curl -s -o /dev/null -m 1 http://127.0.0.1:17710/ && break; sleep 0.5; done
check "storage serve answers" curl -s -o /dev/null -m 2 http://127.0.0.1:17710/
check "root keys written 0600" sh -c 'test "$(stat -f %Lp "$0")" = 600' "$C/storage/credentials.json"
"$R" controller serve --http --auto-approve --listen 127.0.0.1:17720 --domain apps.test --drain 1s --storage http://127.0.0.1:17710 >"$C/controller.log" 2>&1 &
pids+=($!) ctl=$!
for _ in $(seq 50); do curl -s -o /dev/null -m 1 http://127.0.0.1:17720/ && break; sleep 0.2; done
REKCOD_HOME=$N "$R" node join http://127.0.0.1:17720 "$(cat "$C/controller/join-token")" --listen 127.0.0.1:17721 >"$N/node.log" 2>&1 &
pids+=($!)
sleep 2
"$R" remote add admin http://127.0.0.1:17720 --token "$(cat "$C/controller/admin-token")"
cr() { "$R" "$@" --remote admin; }
s3() { curl -s -m 10 --aws-sigv4 "aws:amz:us-east-1:s3" --user "$1" "${@:2}"; }

echo "app buckets"
check "deploy with [storage] bucket = true, 2 instances" sh -c 'cd "$1" && "$0" app deploy --remote admin --name hello --instances 2' "$R" "$repo/w"
check "first instance built, second loaded the stored image" sh -c 'grep -q "hello r1: cloning" "$0" && grep -q "hello r1: loading rekcod-releases/hello/" "$0"' "$N/node.log"
ep=$(sqlite3 "$C/controller/fleet.db" "select endpoint from instances where app = 'hello' and state = 'live' limit 1")
envs=$(curl -s -m 3 "http://$ep/")
key=$(printf '%s\n' "$envs" | sed -n 's/^AWS_ACCESS_KEY_ID=//p') sec=$(printf '%s\n' "$envs" | sed -n 's/^AWS_SECRET_ACCESS_KEY=//p')
check "instance gets S3_ENDPOINT, S3_BUCKET and its keys" sh -c 'printf "%s" "$0" | grep -q "^S3_BUCKET=app-hello" && printf "%s" "$0" | grep -q "^S3_ENDPOINT=http://127.0.0.1:17710" && test -n "$1"' "$envs" "$key"
echo hi >"$repo/obj"
check "its key writes and reads its bucket" sh -c 'curl -sf -m 10 --aws-sigv4 aws:amz:us-east-1:s3 --user "$0" -T "$1" http://127.0.0.1:17710/app-hello/x.txt && test "$(curl -sf -m 10 --aws-sigv4 aws:amz:us-east-1:s3 --user "$0" http://127.0.0.1:17710/app-hello/x.txt)" = hi' "$key:$sec" "$repo/obj"
check "deploy a second app" sh -c 'cd "$1" && "$0" app deploy --remote admin --name other' "$R" "$repo/w"
ep2=$(sqlite3 "$C/controller/fleet.db" "select endpoint from instances where app = 'other' and state = 'live' limit 1")
envs2=$(curl -s -m 3 "http://$ep2/")
key2=$(printf '%s\n' "$envs2" | sed -n 's/^AWS_ACCESS_KEY_ID=//p') sec2=$(printf '%s\n' "$envs2" | sed -n 's/^AWS_SECRET_ACCESS_KEY=//p')
check "a second app's key gets 403 on the first bucket" sh -c 'test "$(curl -s -o /dev/null -w "%{http_code}" -m 10 --aws-sigv4 aws:amz:us-east-1:s3 --user "$0" http://127.0.0.1:17710/app-hello/x.txt)" = 403' "$key2:$sec2"
check "app storage url is a working presigned GET" sh -c 'test "$(curl -s -m 5 "$("$0" app storage --remote admin hello url x.txt)")" = hi' "$R"
check "app storage counts the object" sh -c '"$0" app storage --remote admin hello | grep -q "\"objects\": 1"' "$R"

echo "releases and cache"
v2=$(commit v2)
check "deploy v2" sh -c 'cd "$1" && "$0" app deploy --remote admin --name hello --instances 1' "$R" "$repo/w"
check "build cache stored" sh -c 'curl -sf -m 10 --aws-sigv4 aws:amz:us-east-1:s3 --user "$0" -I http://127.0.0.1:17710/rekcod-cache/hello.tar.zst >/dev/null' \
  "$(sed -n 's/.*"access_key": "\(.*\)".*/\1/p' "$C/storage/credentials.json"):$(sed -n 's/.*"secret_key": "\(.*\)".*/\1/p' "$C/storage/credentials.json")"
lines=$(wc -l <"$N/node.log")
check "rollback" cr app rollback hello
check "rollback loaded the stored image, no clone" sh -c 'tail -n +$(($1 + 1)) "$0" | grep -q "loading rekcod-releases/hello/$2" && ! tail -n +$(($1 + 1)) "$0" | grep -q cloning' "$N/node.log" "$lines" "$v1"
sleep 2
ep=$(sqlite3 "$C/controller/fleet.db" "select endpoint from instances where app = 'hello' and state = 'live' limit 1")
check "rolled back to v1" sh -c 'curl -s -m 3 "http://$0/" | head -1 | grep -qx v1' "$ep"

echo "rotate and destroy"
check "app storage rotate" cr app storage hello rotate
check "the old secret stops working" sh -c 'test "$(curl -s -o /dev/null -w "%{http_code}" -m 10 --aws-sigv4 aws:amz:us-east-1:s3 --user "$0" http://127.0.0.1:17710/app-hello/x.txt)" = 403' "$key:$sec"
check "destroy refuses a non-empty bucket" sh -c '! "$0" app destroy --remote admin hello </dev/null 2>&1 | grep -q destroyed' "$R"
check "destroy --force" cr app destroy hello --force
check "bucket is gone" sh -c 'test "$(curl -s -o /dev/null -w "%{http_code}" -m 10 --aws-sigv4 aws:amz:us-east-1:s3 --user "$0" -I http://127.0.0.1:17710/app-hello)" = 404' \
  "$(sed -n 's/.*"access_key": "\(.*\)".*/\1/p' "$C/storage/credentials.json"):$(sed -n 's/.*"secret_key": "\(.*\)".*/\1/p' "$C/storage/credentials.json")"

echo "backups"
check "controller backup" cr controller backup
check "controller backups lists it" sh -c '"$0" controller backups --remote admin | grep -q "fleet-"' "$R"
kill "$ctl"; wait "$ctl" 2>/dev/null
check "controller restore" "$R" controller restore
"$R" controller serve --http --auto-approve --listen 127.0.0.1:17720 --domain apps.test --drain 1s >>"$C/controller.log" 2>&1 &
pids+=($!)
sleep 2
check "restored controller has the app and remembers storage" sh -c '"$0" app ls --remote admin | grep -q "\"other\"" && grep -c "storage on" "$1" | grep -q 2' "$R" "$C/controller.log"
check "destroy other" cr app destroy other --force

echo
if [[ $failures -eq 0 ]]; then echo "all storage checks passed"; else echo "$failures storage check(s) failed"; exit 1; fi
