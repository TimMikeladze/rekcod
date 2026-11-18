#!/usr/bin/env bash
# Runs install.sh against a file:// release in a temp dir: what it installs, what it
# refuses, and that a failed dashboard copy changes nothing. PATH holds only system
# directories and stubs (a sudo that always fails, gh when a case wants one), so nothing
# outside the temp dir is touched and nothing goes to the network.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'chmod -R u+w "$t" 2>/dev/null; rm -rf "$t"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

# A release: bin/rekcod and the dashboard, its SHA256SUMS.
mkdir -p "$t/src/bin" "$t/src/share/rekcod/dashboard" "$t/rel" "$t/stub" "$t/gh-ok" "$t/gh-bad" "$t/gh-auth"
printf '#!/bin/sh\necho new\n' > "$t/src/bin/rekcod"
chmod +x "$t/src/bin/rekcod"
echo new > "$t/src/share/rekcod/dashboard/server.js"
echo new > "$t/src/share/rekcod/dashboard/start.cjs"
tar -czf "$t/rel/rekcod-darwin-arm64.tar.gz" -C "$t/src" bin share
(cd "$t/rel" && shasum -a 256 rekcod-darwin-arm64.tar.gz > SHA256SUMS)

printf '#!/bin/sh\necho "sudo isn'"'"'t allowed here: $*" >&2\nexit 1\n' > "$t/stub/sudo"
printf '#!/bin/sh\necho "$@" > "%s/gh-args"\nexit 0\n' "$t" > "$t/gh-ok/gh"
printf '#!/bin/sh\necho "Error: verifying with issuer \\"sigstore.dev\\": no matching attestations" >&2\nexit 1\n' > "$t/gh-bad/gh"
printf '#!/bin/sh\necho "To get started with GitHub CLI, please run:  gh auth login" >&2\nexit 4\n' > "$t/gh-auth/gh"
chmod +x "$t/stub/sudo" "$t"/gh-*/gh

# install PREFIX [GH-STUB-DIR]: install.sh's output in $t/out.
install() {
  local path=$t/stub:/usr/bin:/bin:/usr/sbin:/sbin
  [[ -n ${2:-} ]] && path=$2:$path
  PATH=$path PREFIX=$1 REKCOD_RELEASE_URL=file://$t/rel bash "$here/install.sh" > "$t/out" 2>&1
}
old_install() {
  rm -rf "$1"
  mkdir -p "$1/bin" "$1/share/rekcod/dashboard"
  printf '#!/bin/sh\necho old\n' > "$1/bin/rekcod"
  chmod +x "$1/bin/rekcod"
  echo old > "$1/share/rekcod/dashboard/server.js"
  echo old > "$1/share/rekcod/dashboard/start.cjs"
}
is_old() {
  [[ $("$1/bin/rekcod") == old && $(cat "$1/share/rekcod/dashboard/server.js") == old ]] || fail "$2: the old install changed"
}

# Fresh install, no gh: installed, and it says only the checksum was checked.
p=$t/fresh
install "$p" || fail "fresh: exited $?: $(cat "$t/out")"
[[ $("$p/bin/rekcod") == new && $(cat "$p/share/rekcod/dashboard/start.cjs") == new ]] || fail "fresh: not installed"
grep -q "only the checksum was checked" "$t/out" || fail "fresh: didn't say what was checked: $(cat "$t/out")"

# gh verifies: installed, and the provenance check names the repo and the workflow.
p=$t/gh
old_install "$p"
install "$p" "$t/gh-ok" || fail "gh ok: exited $?: $(cat "$t/out")"
[[ $("$p/bin/rekcod") == new ]] || fail "gh ok: not installed"
grep -q -- "--repo TimMikeladze/rekcod --signer-workflow TimMikeladze/rekcod/.github/workflows/release.yml" "$t/gh-args" || fail "gh ok: $(cat "$t/gh-args")"
grep -q "verified: build provenance" "$t/out" || fail "gh ok: $(cat "$t/out")"

# gh finds no valid provenance: nothing installed.
old_install "$p"
if install "$p" "$t/gh-bad"; then fail "bad provenance was installed"; fi
is_old "$p" "bad provenance"

# gh can't check (not logged in): installed, and it says so.
install "$p" "$t/gh-auth" || fail "gh unauthenticated: exited $?: $(cat "$t/out")"
grep -q "gh couldn't check" "$t/out" || fail "gh unauthenticated: $(cat "$t/out")"

# A tarball that doesn't match SHA256SUMS: nothing installed.
old_install "$p"
cp "$t/rel/SHA256SUMS" "$t/sums"
# Change each hash's first digit to a different one (a hash may already start with 0).
awk '{ printf "%s%s\n", (substr($0, 1, 1) == "0" ? "1" : "0"), substr($0, 2) }' "$t/sums" > "$t/rel/SHA256SUMS"
! cmp -s "$t/sums" "$t/rel/SHA256SUMS" || fail "the sums didn't change"
if install "$p"; then fail "a checksum mismatch was installed"; fi
is_old "$p" "checksum mismatch"
cp "$t/sums" "$t/rel/SHA256SUMS"

# share/rekcod can't be written (and sudo fails): the dashboard copy fails before
# anything is replaced, so the old binary and dashboard stay.
old_install "$p"
chmod 555 "$p/share/rekcod"
if [[ $(id -u) != 0 ]]; then
  if install "$p"; then fail "a failed dashboard copy passed"; fi
  is_old "$p" "read-only share"
  grep -q "couldn't copy the dashboard; nothing was installed" "$t/out" || fail "read-only share: $(cat "$t/out")"
fi
chmod 755 "$p/share/rekcod"

# Over a Developer ID-signed rekcod: a new one signed by the same team installs, an
# unsigned one doesn't. Needs a standalone team-signed binary to stand in for both;
# Docker Desktop's CLI is one.
signed=/Applications/Docker.app/Contents/Resources/bin/docker
if [[ -f $signed && $(codesign -dv "$signed" 2>&1) == *TeamIdentifier=[A-Z0-9]* ]]; then
  team_rel() {
    rm -rf "$t/src/bin/rekcod" && cp "$1" "$t/src/bin/rekcod"
    tar -czf "$t/rel/rekcod-darwin-arm64.tar.gz" -C "$t/src" bin share
    (cd "$t/rel" && shasum -a 256 rekcod-darwin-arm64.tar.gz > SHA256SUMS)
  }
  old_install "$p"
  cp "$signed" "$p/bin/rekcod"
  team_rel "$signed"
  install "$p" || fail "same team: exited $?: $(cat "$t/out")"
  grep -q "Developer ID signature of team" "$t/out" || fail "same team: $(cat "$t/out")"
  printf '#!/bin/sh\necho new\n' > "$t/unsigned" && chmod +x "$t/unsigned"
  team_rel "$t/unsigned"
  if install "$p"; then fail "an unsigned binary replaced a team-signed one"; fi
  cmp -s "$signed" "$p/bin/rekcod" || fail "team check: the signed binary changed"
else
  echo "install.sh: no team-signed binary here; skipped the Developer ID case"
fi
echo "install.sh ok"
