#!/usr/bin/env bash
# Release artifacts in out/release:
#   rekcod-<version>-darwin-arm64.tar.gz   bin/rekcod, LICENSE, NOTICE, LICENSES/
#   rekcod-darwin-arm64.tar.gz             the same, for releases/latest/download
#   rekcod-guest-<guest version>.tar.gz    Image, [initramfs], root.img, VERSION, the kernel
#                                          config, the Alpine package list, license notices
#   SHA256SUMS
# Needs out/guest (scripts/build.sh, or GUEST_DIR=... from CI) and runs on macOS.
#
#   scripts/package.sh [--with-dashboard]         both steps below
#   scripts/package.sh build [--with-dashboard]   guest tarball + out/release/stage, unsigned
#   scripts/package.sh sign                       sign stage/bin/rekcod, host tarballs, SHA256SUMS
# release.yml runs the two steps in separate jobs, so the job that holds the signing
# identity never runs cargo or npm.
#
# --with-dashboard also puts the web dashboard's standalone build in share/rekcod/dashboard,
# where `rekcod dashboard` finds it. Off by default: it needs Node on the user's Mac.
#
# CODESIGN_IDENTITY signs the released binary with that identity, hardened runtime and a
# secure timestamp (release.yml sets it to the Developer ID when the secrets exist).
# Default "-": ad-hoc.
set -euo pipefail
step=all with_dashboard=0
for a in "$@"; do
  case $a in
    build | sign) step=$a ;;
    --with-dashboard) with_dashboard=1 ;;
    *) echo "usage: $0 [build|sign] [--with-dashboard]" >&2; exit 2 ;;
  esac
done
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rel=out/release

# Signing rewrites the file, and macOS kills a process whose binary changes underneath it (a
# VM daemon started from target/release), so only copies are signed.
#   signed_into <src> <dest> [identity]
signed_into() {
  local id=${3:--} opts=()
  # Notarization requires the hardened runtime and a timestamp; ad-hoc signing can't have one.
  [[ $id == - ]] || opts=(--options runtime --timestamp)
  cp "$1" "$2.tmp"
  # ${opts[@]+...}: bash 3.2 (macOS /bin/bash) treats an empty array as unset under set -u.
  codesign --force --sign "$id" ${opts[@]+"${opts[@]}"} --entitlements rekcod.entitlements "$2.tmp"
  mv -f "$2.tmp" "$2"
}

build() {
  guest=${GUEST_DIR:-out/guest}
  gv=$(cat "$guest/VERSION")
  rm -rf "$rel" && mkdir -p "$rel/stage/bin"
  cp LICENSE NOTICE "$rel/stage/"
  cp -R LICENSES "$rel/stage/LICENSES"

  guest_tar="rekcod-guest-$gv.tar.gz"
  # COPYFILE_DISABLE: no macOS ._ metadata files in the archive.
  # config and packages.txt: the kernel's .config and the installed Alpine packages, which
  # NOTICE points to for the GPL parts' source.
  COPYFILE_DISABLE=1 tar -C "$guest" -czf "$rel/$guest_tar" $(cd "$guest" && ls Image initramfs root.img VERSION config packages.txt 2>/dev/null) \
    -C "$PWD" LICENSE NOTICE LICENSES
  sha=$(shasum -a 256 "$rel/$guest_tar" | cut -d' ' -f1)

  REKCOD_GUEST_VERSION=$gv REKCOD_GUEST_SHA256=$sha cargo build --locked --release -p rekcod
  cp target/release/rekcod "$rel/stage/bin/rekcod"
  if [[ $with_dashboard == 1 ]]; then
    (cd dashboard && npm ci && npm run bundle)
    mkdir -p "$rel/stage/share/rekcod"
    cp -R dashboard/.next/standalone "$rel/stage/share/rekcod/dashboard"
  fi
}

sign() {
  [[ -f $rel/stage/bin/rekcod ]] || { echo "$0 sign: no $rel/stage/bin/rekcod; run $0 build first" >&2; exit 1; }
  guest_tar=$(cd "$rel" && ls rekcod-guest-*.tar.gz)
  signed_into "$rel/stage/bin/rekcod" "$rel/stage/bin/rekcod" "${CODESIGN_IDENTITY:--}"
  parts=(bin LICENSE NOTICE LICENSES)
  [[ ! -d $rel/stage/share ]] || parts+=(share)
  host_tar="rekcod-$version-darwin-arm64.tar.gz"
  COPYFILE_DISABLE=1 tar -C "$rel/stage" -czf "$rel/$host_tar" "${parts[@]}"
  rm -rf "$rel/stage"
  # Versionless copy so releases/latest/download/rekcod-darwin-arm64.tar.gz always works.
  cp "$rel/$host_tar" "$rel/rekcod-darwin-arm64.tar.gz"
  (cd "$rel" && shasum -a 256 "$host_tar" rekcod-darwin-arm64.tar.gz "$guest_tar" > SHA256SUMS)
}

case $step in
  build) build ;;
  sign) sign ;;
  all)
    build
    sign
    # Leave target/release/rekcod as a dev build again (no embedded guest).
    cargo build --locked --release -p rekcod >/dev/null 2>&1 && signed_into target/release/rekcod target/release/rekcod
    ;;
esac
ls -l "$rel"
