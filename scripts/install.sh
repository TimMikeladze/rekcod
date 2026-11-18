#!/usr/bin/env bash
# Installs a rekcod release: bin/rekcod into $PREFIX/bin, and the web dashboard into
# $PREFIX/share/rekcod/dashboard when the release ships it. Checks the tarball against the
# release's SHA256SUMS first and installs nothing on a mismatch. With gh installed it also
# checks the release workflow's build provenance (`gh attestation verify`), and over a
# Developer ID-signed rekcod it requires the new one to be signed by the same team.
#
#   curl -fsSL https://raw.githubusercontent.com/TimMikeladze/rekcod/main/scripts/install.sh | bash
#
# REKCOD_VERSION=0.3.0   that release instead of the latest
# PREFIX=/usr/local      where to install (sudo only when it isn't writable)
# REKCOD_RELEASE_URL     download from here instead of GitHub (the binary honors it too)
set -euo pipefail

# Everything is in main, called on the last line: a download cut off mid-script runs nothing.
main() {
  local repo=TimMikeladze/rekcod
  local workflow=$repo/.github/workflows/release.yml
  local tarball=rekcod-darwin-arm64.tar.gz
  prefix=${PREFIX:-/usr/local}

  [[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || { echo "rekcod needs an Apple Silicon Mac" >&2; exit 1; }
  local base
  # GitHub's downloads are https only, redirects included; a mirror may be http or file://.
  local strict=(--proto "=https" --tlsv1.2)
  if [[ -n ${REKCOD_RELEASE_URL:-} ]]; then
    base=${REKCOD_RELEASE_URL%/}
    strict=()
  elif [[ -n ${REKCOD_VERSION:-} ]]; then
    base=https://github.com/$repo/releases/download/v${REKCOD_VERSION#v}
  else
    base=https://github.com/$repo/releases/latest/download
  fi
  # No cap on the whole download (it carries the dashboard), but a stalled one is given up.
  fetch() { curl -fL "${strict[@]+"${strict[@]}"}" --connect-timeout 15 --speed-limit 1024 --speed-time 30 --retry 3 "$@"; }

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  echo "rekcod: downloading $base/$tarball"
  fetch --progress-bar -o "$tmp/$tarball" "$base/$tarball"
  fetch -sS -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
  local line
  line=$(grep " $tarball\$" "$tmp/SHA256SUMS") || { echo "rekcod: $tarball isn't listed in SHA256SUMS" >&2; exit 1; }
  (cd "$tmp" && echo "$line" | shasum -a 256 -c -) || { echo "rekcod: checksum mismatch, not installing" >&2; exit 1; }

  # SHA256SUMS comes from the same place as the tarball, so it only catches a damaged
  # download. These two checks don't trust that place.
  local checked=() out
  if command -v gh >/dev/null; then
    local bundle=()
    fetch -sS -o "$tmp/rekcod.sigstore.json" "$base/rekcod.sigstore.json" 2>/dev/null && bundle=(--bundle "$tmp/rekcod.sigstore.json")
    if out=$(gh attestation verify "$tmp/$tarball" --repo "$repo" --signer-workflow "$workflow" "${bundle[@]+"${bundle[@]}"}" 2>&1 </dev/null); then
      checked+=("build provenance from $repo's release workflow")
    elif grep -qiE 'gh auth login|authentication|not logged|http 401|could not resolve|no such host|timeout' <<<"$out"; then
      echo "rekcod: gh couldn't check the build provenance: $(tail -1 <<<"$out")"
    else
      echo "$out" >&2
      echo "rekcod: $tarball has no valid build provenance from $repo's release workflow; not installing it" >&2
      exit 1
    fi
  else
    echo "rekcod: no gh, so the build provenance wasn't checked (\`brew install gh\` adds that check)"
  fi

  mkdir "$tmp/x"
  tar -xzf "$tmp/$tarball" -C "$tmp/x"

  local team=
  # Unsigned (or not a binary at all) prints no TeamIdentifier and fails; that's no team.
  if [[ -x $prefix/bin/rekcod ]]; then team=$({ codesign -dv "$prefix/bin/rekcod" 2>&1 || true; } | sed -n 's/^TeamIdentifier=//p'); fi
  if [[ -n $team && $team != "not set" ]]; then
    if ! codesign --verify --strict -R="=anchor apple generic and certificate leaf[subject.OU] = \"$team\"" "$tmp/x/bin/rekcod" 2>/dev/null; then
      echo "rekcod: the new binary isn't signed by Developer ID team $team, which signed $prefix/bin/rekcod; not installing it" >&2
      exit 1
    fi
    checked+=("Developer ID signature of team $team")
  fi
  if ((${#checked[@]})); then
    local IFS=,
    echo "rekcod: verified: ${checked[*]}"
  else
    echo "rekcod: only the checksum was checked, and it comes from the same release as the download"
  fi

  # The dashboard is copied next to the installed one and checked before anything is
  # replaced: a failed copy (a full disk, say) leaves the old binary and dashboard in place.
  local dash=$prefix/share/rekcod/dashboard staged=
  if [[ -d $tmp/x/share/rekcod/dashboard ]]; then
    as_root mkdir -p "$prefix/share/rekcod"
    as_root rm -rf "$dash.new" "$dash.old"
    if ! as_root cp -R "$tmp/x/share/rekcod/dashboard" "$dash.new" || [[ ! -f $dash.new/server.js || ! -f $dash.new/start.cjs ]]; then
      as_root rm -rf "$dash.new"
      echo "rekcod: couldn't copy the dashboard; nothing was installed" >&2
      exit 1
    fi
    staged=1
  fi

  as_root mkdir -p "$prefix/bin"
  # Copy, then rename over the old binary: overwriting it in place would kill a rekcod daemon
  # still running from it (macOS rejects a binary whose pages change under it).
  as_root cp "$tmp/x/bin/rekcod" "$prefix/bin/.rekcod.new"
  as_root chmod 0755 "$prefix/bin/.rekcod.new"
  as_root mv -f "$prefix/bin/.rekcod.new" "$prefix/bin/rekcod"
  as_root xattr -d com.apple.quarantine "$prefix/bin/rekcod" 2>/dev/null || true
  echo "rekcod: installed $prefix/bin/rekcod"

  if [[ -n $staged ]]; then
    if [[ -e $dash ]]; then as_root mv "$dash" "$dash.old"; fi
    if ! as_root mv "$dash.new" "$dash"; then
      if [[ -e $dash.old ]]; then as_root mv "$dash.old" "$dash"; fi
      echo "rekcod: couldn't move the new dashboard into place; $dash is the old one" >&2
      exit 1
    fi
    as_root rm -rf "$dash.old"
    echo "rekcod: installed the dashboard in $dash (\`rekcod dashboard\`, needs Node)"
  fi

  case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "rekcod: $prefix/bin isn't on your PATH" ;;
  esac
  echo "rekcod: next, \`rekcod start\` (the first run downloads and verifies the guest image, ~100 MB)"
}

# sudo only for what the user can't write: /usr/local is root-owned on a fresh Mac.
as_root() {
  local d=$prefix
  while [[ ! -e $d ]]; do d=$(dirname "$d"); done
  if [[ -w $d && (! -e $prefix/bin || -w $prefix/bin) ]]; then "$@"; else sudo "$@"; fi
}

main "$@"
