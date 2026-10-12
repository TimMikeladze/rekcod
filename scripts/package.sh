#!/usr/bin/env bash
# Release artifacts in out/release (docs/phase3.md):
#   rekcod-<version>-darwin-arm64.tar.gz   bin/rekcod, with the guest version + sha embedded
#   rekcod-darwin-arm64.tar.gz             the same, for releases/latest/download
#   rekcod-guest-<guest version>.tar.gz    Image, [initramfs], root.img, VERSION
#   SHA256SUMS
# Needs out/guest (scripts/build.sh, or GUEST_DIR=... from CI) and runs on macOS.
set -euo pipefail
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

REKCOD_GUEST_VERSION=$gv REKCOD_GUEST_SHA256=$sha cargo build --release -p rekcod
codesign --force --sign - --entitlements rekcod.entitlements target/release/rekcod
cp target/release/rekcod "$rel/stage/bin/"
host_tar="rekcod-$version-darwin-arm64.tar.gz"
COPYFILE_DISABLE=1 tar -C "$rel/stage" -czf "$rel/$host_tar" bin
rm -rf "$rel/stage"
# Versionless copy so releases/latest/download/rekcod-darwin-arm64.tar.gz always works.
cp "$rel/$host_tar" "$rel/rekcod-darwin-arm64.tar.gz"
(cd "$rel" && shasum -a 256 "$host_tar" rekcod-darwin-arm64.tar.gz "$guest_tar" > SHA256SUMS)
# Leave target/release/rekcod as a dev build again (no embedded guest).
cargo build --release -p rekcod >/dev/null 2>&1 && codesign --force --sign - --entitlements rekcod.entitlements target/release/rekcod
ls -l "$rel"
