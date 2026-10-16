#!/usr/bin/env bash
# Release artifacts in out/release (docs/phase3.md):
#   rekcod-<version>-darwin-arm64.tar.gz   bin/rekcod, with the guest version + sha embedded
#   rekcod-darwin-arm64.tar.gz             the same, for releases/latest/download
#   rekcod-guest-<guest version>.tar.gz    Image, [initramfs], root.img, VERSION
#   SHA256SUMS
# Needs out/guest (scripts/build.sh, or GUEST_DIR=... from CI) and runs on macOS.
#
# --with-dashboard also puts the web dashboard's standalone build in share/rekcod/dashboard,
# where `rekcod dashboard` finds it. Off by default: it needs Node on the user's Mac.
set -euo pipefail
with_dashboard=0
[[ ${1:-} == --with-dashboard ]] && with_dashboard=1
cd "$(dirname "$0")/.."
guest=${GUEST_DIR:-out/guest}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
gv=$(cat "$guest/VERSION")
rel=out/release
rm -rf "$rel" && mkdir -p "$rel/stage/bin"

guest_tar="rekcod-guest-$gv.tar.gz"
# COPYFILE_DISABLE: no macOS ._ metadata files in the archive.
COPYFILE_DISABLE=1 tar -C "$guest" -czf "$rel/$guest_tar" $(cd "$guest" && ls Image initramfs root.img VERSION 2>/dev/null)
sha=$(shasum -a 256 "$rel/$guest_tar" | cut -d' ' -f1)

# Signing rewrites the file, and macOS kills a process whose binary changes underneath it (a
# VM daemon started from target/release), so only copies are signed.
signed_into() {
  cp target/release/rekcod "$1.tmp"
  codesign --force --sign - --entitlements rekcod.entitlements "$1.tmp"
  mv -f "$1.tmp" "$1"
}
REKCOD_GUEST_VERSION=$gv REKCOD_GUEST_SHA256=$sha cargo build --release -p rekcod
signed_into "$rel/stage/bin/rekcod"
parts=(bin)
if [[ $with_dashboard == 1 ]]; then
  (cd dashboard && npm ci && npm run bundle)
  mkdir -p "$rel/stage/share/rekcod"
  cp -R dashboard/.next/standalone "$rel/stage/share/rekcod/dashboard"
  parts+=(share)
fi
host_tar="rekcod-$version-darwin-arm64.tar.gz"
COPYFILE_DISABLE=1 tar -C "$rel/stage" -czf "$rel/$host_tar" "${parts[@]}"
rm -rf "$rel/stage"
# Versionless copy so releases/latest/download/rekcod-darwin-arm64.tar.gz always works.
cp "$rel/$host_tar" "$rel/rekcod-darwin-arm64.tar.gz"
(cd "$rel" && shasum -a 256 "$host_tar" rekcod-darwin-arm64.tar.gz "$guest_tar" > SHA256SUMS)
# Leave target/release/rekcod as a dev build again (no embedded guest).
cargo build --release -p rekcod >/dev/null 2>&1 && signed_into target/release/rekcod
ls -l "$rel"
