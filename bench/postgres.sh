#!/usr/bin/env bash
# Postgres under each docker context: pgbench run inside the container (disk + CPU)
# and from the Mac through the published port (adds the port-forwarding path).
# Containers are capped at the same CPUs and memory so the VM sizes don't decide it.
# The VM's vCPU count still matters: this ping-pong workload loses throughput per extra
# vCPU (cross-vCPU wakeups). On an M5 Pro, read-only: 4 vCPU 160k, 6 130k, 8 113k TPS.
#
#   bench/postgres.sh rekcod desktop-linux     # contexts to compare
#   SCALE=20 CLIENTS=8 SECS=30 CPUS=4 MEM=3g bench/postgres.sh rekcod
set -euo pipefail
SCALE=${SCALE:-20} CLIENTS=${CLIENTS:-8} SECS=${SECS:-30} CPUS=${CPUS:-4} MEM=${MEM:-3g}
IMAGE=postgres:17-alpine NAME=rekcod-pgbench PORT=${PORT:-55432}

tps() { sed -n 's/^tps = \([0-9.]*\).*/\1/p' | cut -d. -f1; }

for ctx in "${@:-rekcod}"; do
  d() { docker --context "$ctx" "$@"; }
  d rm -f "$NAME" >/dev/null 2>&1 || true
  d volume rm "$NAME" >/dev/null 2>&1 || true
  d pull -q "$IMAGE" >/dev/null
  d run -d --name "$NAME" --cpus "$CPUS" --memory "$MEM" -p "127.0.0.1:$PORT:5432" \
    -e POSTGRES_PASSWORD=bench -v "$NAME:/var/lib/postgresql/data" "$IMAGE" >/dev/null
  until d exec "$NAME" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1; do sleep 0.2; done
  sleep 1

  t0=$(python3 -c 'import time;print(time.time())')
  d exec "$NAME" pgbench -U postgres -i -q -s "$SCALE" postgres >/dev/null 2>&1
  init=$(python3 -c "import time;print(round(time.time()-$t0,1))")

  pb() { d exec "$NAME" pgbench -U postgres -c "$CLIENTS" -j 4 -T "$SECS" "$@" postgres 2>/dev/null | tps; }
  rw=$(pb) ro=$(pb -S)
  mac=$(PGPASSWORD=bench pgbench -h 127.0.0.1 -p "$PORT" -U postgres -c "$CLIENTS" -j 4 \
    -T "$SECS" -S postgres 2>/dev/null | tps)

  printf '%-14s init %5ss  rw %6s tps  ro %6s tps  ro-from-mac %6s tps\n' "$ctx" "$init" "$rw" "$ro" "$mac"
  d rm -f "$NAME" >/dev/null
  d volume rm "$NAME" >/dev/null
done
