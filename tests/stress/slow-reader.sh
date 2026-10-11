#!/usr/bin/env bash
# Regression test for the VM freeze (docs/phase2b-4.md): a Mac-side client
# that stops reading a stream (here `docker logs -f` into a pipe nobody reads)
# must not stall the guest. Before the fix, Virtualization.framework blocked in
# writev on the vsock queue and every vCPU waited behind it.
set -uo pipefail
ctx=${CONTEXT:-rekcod}
d=(docker --context "$ctx")
"${d[@]}" rm -f rekcod-chatty >/dev/null 2>&1
"${d[@]}" run -d --name rekcod-chatty alpine:3.22 sh -c 'yes "$(head -c 200 /dev/zero | tr "\0" x)"' >/dev/null
trap '"${d[@]}" rm -f rekcod-chatty >/dev/null 2>&1; kill $reader 2>/dev/null' EXIT
"${d[@]}" logs -f rekcod-chatty | sleep 600 & reader=$!
fail=0
for i in 1 2 3 4 5 6; do
  sleep 5
  if ! timeout 20 "${d[@]}" run --rm alpine:3.22 true; then echo "FAIL: engine unresponsive after $((i * 5))s of a stalled reader"; fail=1; break; fi
done
[ $fail = 0 ] && echo "ok: engine responsive for 30s with a stalled log reader"
exit $fail
