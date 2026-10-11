#!/usr/bin/env bash
# Runs docker/compose's own e2e suite (pkg/e2e, plugin mode) against one or more
# docker contexts and prints per-context pass/fail counts. Contexts run one after
# another: they publish the same Mac ports.
#
#   tests/stress/compose-e2e.sh rekcod orbstack
# Needs go. The checkout matches `docker compose version` and is cached in
# ~/.cache/rekcod/compose-<version>. Reports land in $OUT (default: a temp dir).
set -uo pipefail
contexts=("${@:-rekcod}")
v=$(docker compose version --short)
src="$HOME/.cache/rekcod/compose-$v"
out=${OUT:-$(mktemp -d)}; mkdir -p "$out"
if [[ ! -d "$src" ]]; then
  mkdir -p "$(dirname "$src")"
  git clone -q --depth 1 --branch "v$v" https://github.com/docker/compose.git "$src" || exit 1
fi
(cd "$src" && make -s build example-provider >/dev/null) || { echo "compose build failed"; exit 1; }

for ctx in "${contexts[@]}"; do
  echo "== $ctx (compose v$v)"
  (cd "$src" && DOCKER_CONTEXT="$ctx" go test -tags e2e -count=1 -timeout 90m ./pkg/e2e -v 2>&1) >"$out/$ctx.log"
  pass=$(grep -c '^--- PASS' "$out/$ctx.log")
  fail=$(grep -c '^--- FAIL' "$out/$ctx.log")
  skip=$(grep -c '^--- SKIP' "$out/$ctx.log")
  echo "   top-level tests: $pass passed, $fail failed, $skip skipped (log: $out/$ctx.log)"
  grep '^--- FAIL' "$out/$ctx.log" | sed 's/^/   /'
done
