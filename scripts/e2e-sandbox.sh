#!/usr/bin/env bash
# Agent sandbox checks (docs/agent-sandboxes.md). Creates and removes its own sandboxes
# only and leaves the main VM alone, so it's safe to run while your containers are up.
# scripts/e2e.sh runs it too.
set -uo pipefail
R=${REKCOD:-$(cd "$(dirname "$0")/.." && pwd)/target/release/rekcod}
failures=0
ok() { printf '  ok    %s\n' "$1"; }
bad() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
check() { local name=$1; shift; if "$@" >/dev/null 2>&1; then ok "$name"; else bad "$name"; fi; }

echo "agent sandboxes"
p=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-e2e-sb-XXXXXX")
p=$(cd "$p" && pwd -P)
printf keep >"$p/keep.txt"
printf old >"$p/gone.txt"
touch -t 202001010000 "$p/keep.txt" "$p/gone.txt"
names=()
for i in 1 2 3 4 5 6 7 8; do names+=("e2e-sb$i"); done

check "create" "$R" sandbox create "$p" --name e2e-sb1 --image alpine:3.22
check "exit code passes through" sh -c '"$0" sandbox exec e2e-sb1 -- sh -c "exit 7"; test $? = 7' "$R"
check "unknown sandbox exits 2" sh -c '"$0" sandbox exec e2e-nope -- true; test $? = 2' "$R"
check "only the project is visible" sh -c '! "$0" sandbox exec e2e-sb1 -- ls "$HOME"' "$R"
"$R" sandbox exec e2e-sb1 -- sh -c 'echo new > keep.txt; mv gone.txt /tmp/; echo added > added.txt' >/dev/null 2>&1
check "copy: the real project is untouched" sh -c 'test "$(cat "$0/keep.txt")" = keep && test -f "$0/gone.txt"' "$p"
check "diff lists A/M/D" sh -c 'test "$("$0" sandbox diff e2e-sb1 --stat | tr "\n" " ")" = "A added.txt D gone.txt M keep.txt "' "$R"
echo mac >"$p/keep.txt"
check "apply refuses a file changed on the Mac" sh -c '! "$0" sandbox apply e2e-sb1' "$R"
printf keep >"$p/keep.txt"
touch -t 202001010000 "$p/keep.txt"
check "apply writes the changes" sh -c '"$0" sandbox apply e2e-sb1 && test "$(cat "$1/keep.txt")" = new && test ! -e "$1/gone.txt"' "$R" "$p"
check "egress: allowed registry" "$R" sandbox exec e2e-sb1 -- wget -q -T 10 -O /dev/null https://dl-cdn.alpinelinux.org/alpine/
check "egress: other names refused" sh -c '! "$0" sandbox exec e2e-sb1 -- wget -q -T 5 -O /dev/null https://example.com' "$R"
check "egress: direct IPs rejected" sh -c '! "$0" sandbox exec e2e-sb1 -- nc -z -w 3 1.1.1.1 443' "$R"
check "egress: denial logged" sh -c '"$0" sandbox log e2e-sb1 --net | grep -q example.com' "$R"
check "Mac unreachable" sh -c '! "$0" sandbox exec e2e-sb1 -- nc -z -w 2 192.168.64.1 22' "$R"
"$R" sandbox exec e2e-sb1 -- sh -c 'echo parent > forked.txt' >/dev/null 2>&1
check "fork copies the workspace" sh -c '"$0" sandbox fork e2e-sb1 --name e2e-sb2 && test "$("$0" sandbox exec e2e-sb2 -- cat forked.txt)" = parent' "$R"
"$R" sandbox exec e2e-sb2 -- touch child-only >/dev/null 2>&1
check "fork: child writes stay in the child" sh -c '! "$0" sandbox exec e2e-sb1 -- test -e child-only' "$R"
check "stop, then exec boots it again" sh -c '"$0" sandbox stop e2e-sb2 && "$0" sandbox exec e2e-sb2 -- true' "$R"
for i in 3 4 5 6 7 8; do "$R" sandbox create "$p" --name "e2e-sb$i" --image alpine:3.22 --memory 1 >/dev/null 2>&1 & done
wait
check "8 sandboxes at once" sh -c 'test "$("$0" sandbox ls | grep -c "e2e-sb. .*running")" = 8' "$R"
check "mcp lists tools" sh -c 'echo "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}" | "$0" mcp | grep -q sandbox_exec' "$R"
check "rm deletes them" sh -c '"$0" sandbox rm "${@:1}" && ! "$0" sandbox ls | grep -q e2e-sb' "$R" "${names[@]}"

echo
if [[ $failures -eq 0 ]]; then echo "all sandbox checks passed"; else echo "$failures sandbox check(s) failed"; exit 1; fi
