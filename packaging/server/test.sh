#!/usr/bin/env bash
# Runs postinstall against a stub rekcod and stubs for the system commands it calls, as this
# user and through its root branch, and checks the pairing line it leaves. CI runs it on
# macOS; it touches nothing outside a temp dir.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
# A space in the home path: dscl output has to be read whole.
home="$t/home dir"
mkdir -p "$t/vol/usr/local/bin" "$t/vol/usr/local/share/rekcod" "$home/Desktop" "$t/stub"
echo apps.example.com > "$t/vol/usr/local/share/rekcod/server-domain"
cat > "$t/vol/usr/local/bin/rekcod" <<EOF
#!/bin/sh
echo "\$*" >> "$t/calls"
[ "\$1 \$2" = "server setup" ] || exit 0
[ -n "\${FAIL:-}" ] && exit 1
echo "storage installed: runs now and at login"
echo "remote https://rekcod-3f9a1c2b7d4e.bonjour tok123 sha256:ff"
EOF
stub() { printf '#!/bin/sh\n%s\n' "$2" > "$t/stub/$1"; }
stub scutil 'echo Tims-Mac-mini'
stub open "echo \"open \$*\" >> '$t/calls'"
stub pbcopy "cat > '$t/clip'"
# The root branch's commands: the console owner, their uid and home, then run as them.
stub stat "echo \${CONSOLE:-tim}"
stub id 'echo 501'
stub dscl "echo 'NFSHomeDirectory: $home'"
stub launchctl 'shift 2; exec "$@"'
stub sudo 'shift 3; exec "$@"'
stub pmset "echo \"pmset \$*\" >> '$t/calls'"
stub chown 'true'
chmod +x "$t/vol/usr/local/bin/rekcod" "$t/stub/"*
run() { HOME=$home PATH=$t/stub:$PATH REKCOD_PKG_LOG=$t/log bash "$here/scripts/postinstall" pkg / "$t/vol" >/dev/null; }
fail() { echo "FAIL: $*" >&2; exit 1; }
want="rekcod remote add tims-mac-mini https://rekcod-3f9a1c2b7d4e.bonjour --token tok123 --ca-fingerprint sha256:ff"
pair=$home/Desktop/rekcod-pair.txt

check() {
  grep -qx "server setup --domain apps.example.com --no-power" "$t/calls" || fail "$1: setup args: $(cat "$t/calls")"
  grep -qxF "$want" "$pair" || fail "$1: pair file: $(cat "$pair")"
  [[ $(stat -f %Lp "$pair") == 600 ]] || fail "$1: pair file isn't 0600"
  [[ $(cat "$t/clip") == "$want" ]] || fail "$1: clipboard: $(cat "$t/clip")"
  ! grep -q tok123 "$t/log" || fail "$1: the token is in the log"
}

# A pair file from an earlier install, world-readable: it must come back 0600.
echo old > "$pair" && chmod 644 "$pair"
run || fail "as user: exited $?"
check "as user"
! grep -q "at-boot" "$t/calls" || fail "as user: at-boot without root"

rm -f "$t/calls" "$t/log"
REKCOD_PKG_TEST_ROOT=1 run || fail "as root: exited $?"
check "as root"
grep -q "^pmset -a sleep 0" "$t/calls" || fail "as root: no pmset"
grep -qx "server at-boot --user tim" "$t/calls" || fail "as root: no at-boot: $(cat "$t/calls")"

# Setup Assistant owns the console: pmset still runs, setup doesn't.
rm -f "$t/calls"
CONSOLE=_mbsetupuser REKCOD_PKG_TEST_ROOT=1 run || fail "no console user: exited $?"
grep -q "^pmset" "$t/calls" || fail "no console user: no pmset"
! grep -q "server setup" "$t/calls" || fail "no console user: setup ran"

# No Desktop: falls back to the home folder.
rm -rf "$home/Desktop" "$t/calls"
run || fail "no Desktop: exited $?"
grep -qxF "$want" "$home/rekcod-pair.txt" || fail "no Desktop: no fallback pair file"

# A failed setup fails the install.
if FAIL=1 run; then fail "a failed setup passed"; fi
echo "postinstall ok"
