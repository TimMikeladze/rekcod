#!/usr/bin/env bash
# Installs a rekcod release: bin/rekcod into $PREFIX/bin, and the web dashboard into
# $PREFIX/share/rekcod/dashboard when the release ships it. Checks the tarball against the
# release's SHA256SUMS first and installs nothing on a mismatch.
#
#   curl -fsSL https://raw.githubusercontent.com/TimMikeladze/rekcod/main/scripts/install.sh | bash
#
# REKCOD_VERSION=0.3.0   that release instead of the latest
# PREFIX=/usr/local      where to install (sudo only when it isn't writable)
# REKCOD_RELEASE_URL     download from here instead of GitHub (the binary honors it too)
set -euo pipefail

repo=TimMikeladze/rekcod
prefix=${PREFIX:-/usr/local}
tarball=rekcod-darwin-arm64.tar.gz

[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || { echo "rekcod needs an Apple Silicon Mac" >&2; exit 1; }
if [[ -n ${REKCOD_RELEASE_URL:-} ]]; then
  base=${REKCOD_RELEASE_URL%/}
elif [[ -n ${REKCOD_VERSION:-} ]]; then
  base=https://github.com/$repo/releases/download/v${REKCOD_VERSION#v}
else
  base=https://github.com/$repo/releases/latest/download
fi

# sudo only for what the user can't write: /usr/local is root-owned on a fresh Mac.
as_root() {
  local d=$prefix
  while [[ ! -e $d ]]; do d=$(dirname "$d"); done
  if [[ -w $d && (! -e $prefix/bin || -w $prefix/bin) ]]; then "$@"; else sudo "$@"; fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "rekcod: downloading $base/$tarball"
curl -fL --progress-bar -o "$tmp/$tarball" "$base/$tarball"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
line=$(grep " $tarball\$" "$tmp/SHA256SUMS") || { echo "rekcod: $tarball isn't listed in SHA256SUMS" >&2; exit 1; }
(cd "$tmp" && echo "$line" | shasum -a 256 -c -) || { echo "rekcod: checksum mismatch, not installing" >&2; exit 1; }

mkdir "$tmp/x"
tar -xzf "$tmp/$tarball" -C "$tmp/x"
as_root mkdir -p "$prefix/bin"
# Copy, then rename over the old binary: overwriting it in place would kill a rekcod daemon
# still running from it (macOS rejects a binary whose pages change under it).
as_root cp "$tmp/x/bin/rekcod" "$prefix/bin/.rekcod.new"
as_root chmod 0755 "$prefix/bin/.rekcod.new"
as_root mv -f "$prefix/bin/.rekcod.new" "$prefix/bin/rekcod"
as_root xattr -d com.apple.quarantine "$prefix/bin/rekcod" 2>/dev/null || true
echo "rekcod: installed $prefix/bin/rekcod"

if [[ -d $tmp/x/share/rekcod/dashboard ]]; then
  as_root mkdir -p "$prefix/share/rekcod"
  as_root rm -rf "$prefix/share/rekcod/dashboard"
  as_root cp -R "$tmp/x/share/rekcod/dashboard" "$prefix/share/rekcod/dashboard"
  echo "rekcod: installed the dashboard in $prefix/share/rekcod/dashboard (\`rekcod dashboard\`, needs Node)"
fi

case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) echo "rekcod: $prefix/bin isn't on your PATH" ;;
esac
echo "rekcod: next, \`rekcod start\` (the first run downloads and verifies the guest image, ~100 MB)"
