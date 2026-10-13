#!/usr/bin/env bash
# macOS sandbox checks (docs/platform.md, track 4). Needs a ready base image:
#   rekcod macos fetch && rekcod macos install && rekcod macos setup   (once, by hand)
# Creates and removes its own sandboxes only.
set -uo pipefail
R=${REKCOD:-$(cd "$(dirname "$0")/.." && pwd)/target/release/rekcod}
failures=0
ok() { printf '  ok    %s\n' "$1"; }
bad() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
check() {
  local name=$1 out; shift
  if out=$("$@" 2>&1); then ok "$name"; else bad "$name"; printf '%s\n' "$out" | tail -3 | sed 's/^/        /'; fi
}
if ! "$R" macos ls | grep -q '^default .*ready'; then
  echo "skipped: no ready macOS image (rekcod macos ls)"
  exit 0
fi

echo "macOS sandboxes"
p=$(mktemp -d "${TMPDIR:-/tmp}/rekcod-e2e-mac-XXXXXX")
printf old >"$p/keep.txt"
touch -t 202001010000 "$p/keep.txt"
trap '"$R" sandbox rm e2e-mac1 e2e-mac2 >/dev/null 2>&1' EXIT

check "create" "$R" sandbox create "$p" --os macos --name e2e-mac1
check "exec runs macOS" sh -c '"$0" sandbox exec e2e-mac1 -- sw_vers -productName | grep -q macOS' "$R"
check "exit code passes through" sh -c '"$0" sandbox exec e2e-mac1 -- sh -c "exit 7"; test $? = 7' "$R"
check "workspace copied in" sh -c 'test "$("$0" sandbox exec e2e-mac1 -- cat keep.txt)" = old' "$R"
"$R" sandbox exec e2e-mac1 -- sh -c 'echo new > keep.txt' >/dev/null 2>&1
check "diff sees guest edits" sh -c 'test "$("$0" sandbox diff e2e-mac1 --stat)" = "M keep.txt"' "$R"
check "apply" sh -c '"$0" sandbox apply e2e-mac1 && test "$(cat "$1/keep.txt")" = new' "$R" "$p"
check "screenshot is a PNG" sh -c '"$0" sandbox screenshot e2e-mac1 "$1/s.png" && file "$1/s.png" | grep -q PNG' "$R" "$p"
check "input types" "$R" sandbox input e2e-mac1 key escape
check "fork" sh -c '"$0" sandbox fork e2e-mac1 --name e2e-mac2 && "$0" sandbox exec e2e-mac2 -- true' "$R"
check "a third macOS VM is refused (licence)" sh -c '! "$0" sandbox create "$1" --os macos' "$R" "$p"
check "rm" "$R" sandbox rm e2e-mac1 e2e-mac2

echo
if [[ $failures -eq 0 ]]; then echo "all macOS checks passed"; else echo "$failures macOS check(s) failed"; exit 1; fi
