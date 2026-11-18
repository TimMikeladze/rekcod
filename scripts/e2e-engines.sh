#!/usr/bin/env bash
# Checks `rekcod host state` and `rekcod host exec` without a second Mac: REKCOD_SSH points
# at a stand-in that runs the remote command on this Mac, and a throwaway DOCKER_CONFIG holds
# the host's docker context, so your own contexts aren't touched.
#   scripts/e2e-engines.sh            # with target/release/rekcod
#   REKCOD=target/debug/rekcod scripts/e2e-engines.sh
# With REKCOD_HOME set to an empty directory there is no VM, and only the stopped branch runs
# (ci.yml does this); with a running VM it also runs a container there.
set -euo pipefail
cd "$(dirname "$0")/.."
REKCOD=$(cd "$(dirname "${REKCOD:-target/release/rekcod}")" && pwd)/$(basename "${REKCOD:-target/release/rekcod}")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export DOCKER_CONFIG=$tmp/docker
mkdir -p "$DOCKER_CONFIG"
# Plugins (compose, buildx) from your config, so passthrough commands still find them.
[[ -d $HOME/.docker/cli-plugins ]] && ln -s "$HOME/.docker/cli-plugins" "$DOCKER_CONFIG/cli-plugins"

# ssh's last argument is the remote command line; run it here, this rekcod first on PATH.
cat >"$tmp/ssh" <<EOF
#!/bin/sh
for last; do :; done
PATH="$(dirname "$REKCOD"):\$PATH" exec /bin/sh -c "\$last"
EOF
chmod +x "$tmp/ssh"
export REKCOD_SSH=$tmp/ssh
docker context create rekcod-loop --description "rekcod host" --docker host=ssh://me@loop >/dev/null 2>&1

fail() { echo "e2e-engines: $*" >&2; exit 1; }
"$REKCOD" host ls | grep -q loop || fail "host ls doesn't list loop"

"$REKCOD" host state loop >"$tmp/state.json" || fail "host state loop failed"
python3 - "$tmp/state.json" <<'EOF' || fail "host state isn't rewritten"
import json, sys
s = json.load(open(sys.argv[1]))
assert s["engine"] == "loop", s.get("engine")
for k in ("hosts", "remotes", "apps", "jobs", "local"):
    assert k not in s, f"{k} is this Mac's, not the host's"
def actions(v):
    if isinstance(v, list):
        for x in v: yield from actions(x)
    elif isinstance(v, dict):
        if isinstance(v.get("kind"), str) and "label" in v: yield v
        else:
            for x in v.values(): yield from actions(x)
acts = list(actions(s))
assert acts, "no actions"
for a in acts:
    assert "url" not in a and a["argv"][:3] == ["host", "exec", "loop"] and "--" in a["argv"], a
    assert a["kind"] not in ("DashboardOpen", "Uninstall", "HostUse"), a["kind"]
print(f"host state: {len(acts)} actions, each through host exec; running={s['running']}")
EOF

# A plain command, a working directory, and quoting that survives the remote shell.
[[ $("$REKCOD" host exec loop -- --version) == rekcod* ]] || fail "host exec --version"
"$REKCOD" host exec loop --dir "$tmp" -- status --json | python3 -c 'import json,sys; json.load(sys.stdin)' || fail "host exec status --json"
running() { "$REKCOD" status --json | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin)["running"] else 1)'; }
if ! running; then
  # A stopped VM there stays stopped for the dashboard's followers.
  out=$(REKCOD_NO_BOOT=1 "$REKCOD" host exec loop -- ps 2>&1) && fail "ps booted a stopped VM: $out"
  grep -q REKCOD_NO_BOOT <<<"$out" || fail "REKCOD_NO_BOOT didn't reach the host: $out"
else
  "$REKCOD" host exec loop -- ps --format '{{.Names}}' >/dev/null || fail "host exec ps"
  "$REKCOD" host exec loop -- run --rm alpine:3.24 echo "it's quoted" | grep -qx "it's quoted" || fail "quoting"
fi
echo "e2e-engines: ok"
