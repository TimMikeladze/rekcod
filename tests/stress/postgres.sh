#!/usr/bin/env bash
# Postgres with PGDATA on a Mac bind mount: pgbench load, a restart (clean and
# killed), then amcheck + row counts. Exercises fsync, rename, truncate and
# page-cache coherence through the cached virtio-fs mount.
#
#   tests/stress/postgres.sh            # CONTEXT=orbstack to compare
set -uo pipefail
ctx=${CONTEXT:-rekcod}
d=(docker --context "$ctx")
root=$(mktemp -d "$HOME/.rekcod-pg.XXXX")
name=rekcod-pg
trap '"${d[@]}" rm -f $name >/dev/null 2>&1; rm -rf "$root"' EXIT
fail=0
ok() { echo "  ok    $1"; }
bad() { echo "  FAIL  $1"; fail=1; }

start() {
  "${d[@]}" rm -f $name >/dev/null 2>&1
  "${d[@]}" run -d --name $name -e POSTGRES_PASSWORD=pw -e PGDATA=/pg/data \
    -v "$root:/pg" postgres:17-alpine >/dev/null
  for _ in $(seq 1 120); do
    "${d[@]}" exec $name pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  "${d[@]}" logs $name 2>&1 | tail -20
  return 1
}
psql() { "${d[@]}" exec $name psql -U postgres -h 127.0.0.1 -tAc "$1"; }

echo "postgres on a bind mount ($ctx)"
start && ok "initdb + start" || { bad "initdb + start"; exit 1; }
t0=$(date +%s)
"${d[@]}" exec $name pgbench -U postgres -h 127.0.0.1 -i -s 10 -q postgres >/dev/null 2>&1 && ok "pgbench -i -s 10 ($(( $(date +%s) - t0 ))s)" || bad "pgbench -i"
tps=$("${d[@]}" exec $name pgbench -U postgres -h 127.0.0.1 -c 8 -j 4 -T 30 postgres 2>&1 | awk '/^tps/ {print int($3); exit}')
[ -n "$tps" ] && ok "pgbench 30s: $tps tps" || bad "pgbench run"
want=$(psql "select count(*) || ':' || sum(abalance) from pgbench_accounts")

"${d[@]}" restart $name >/dev/null && start_ok=1
for _ in $(seq 1 60); do "${d[@]}" exec $name pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && break; sleep 0.5; done
[ "$(psql "select count(*) || ':' || sum(abalance) from pgbench_accounts")" = "$want" ] && ok "data intact after clean restart" || bad "data after clean restart"

"${d[@]}" exec $name pgbench -U postgres -h 127.0.0.1 -c 4 -T 5 postgres >/dev/null 2>&1
want=$(psql "select count(*) || ':' || sum(abalance) from pgbench_accounts")
"${d[@]}" kill $name >/dev/null   # SIGKILL: crash recovery from WAL on the next start
"${d[@]}" start $name >/dev/null
for _ in $(seq 1 60); do "${d[@]}" exec $name pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && break; sleep 0.5; done
[ "$(psql "select count(*) || ':' || sum(abalance) from pgbench_accounts")" = "$want" ] && ok "committed data intact after kill -9 + recovery" || bad "data after crash recovery"

psql "create extension if not exists amcheck" >/dev/null
chk=$(psql "select count(*) from (select bt_index_check(c.oid, true) from pg_index i join pg_class c on c.oid = i.indexrelid join pg_am a on a.oid = c.relam where a.amname = 'btree' and c.relnamespace = 'public'::regnamespace) s" 2>&1)
[[ "$chk" =~ ^[0-9]+$ ]] && ok "amcheck: $chk btree indexes verified" || bad "amcheck: $chk"
exit $fail
