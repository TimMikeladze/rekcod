#!/usr/bin/env bash
# Build rekcod: host binary (signed for Virtualization.framework), guest agent,
# and the guest image (out/guest). The guest image is built by guest/build.sh
# inside the running rekcod VM (docs/guest-build.md), so `rekcod start` first.
# KERNEL, KERNEL_REPO, FUSE_PATCH and the guest/versions.env overrides pass through.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --locked --release -p rekcod
cargo build --locked --release -p rekcod-agent --target aarch64-unknown-linux-musl
# Sign a copy and move it in: signing rewrites the file, and macOS kills a VM daemon still
# running from the old one.
cp target/release/rekcod target/release/rekcod.signing
codesign --force --sign - --entitlements rekcod.entitlements target/release/rekcod.signing
mv -f target/release/rekcod.signing target/release/rekcod
mkdir -p out/agent && cp target/aarch64-unknown-linux-musl/release/rekcod-agent out/agent/

if [[ "${1:-}" != "--host-only" ]]; then
  rekcod=${REKCOD:-$(command -v rekcod || echo target/release/rekcod)}
  "$rekcod" status >/dev/null 2>&1 || { echo "rekcod isn't running: run \`rekcod start\` first (or pass --host-only)" >&2; exit 1; }
  # The repo is at the same path inside the VM; work files go on its data disk.
  # Empty settings fall back to guest/build.sh's defaults and guest/versions.env.
  "$rekcod" shell <<EOF
export REKCOD_GUEST_WORK=/var/lib/docker/.rekcod-guest-build
export KERNEL='${KERNEL:-}' KERNEL_REPO='${KERNEL_REPO:-}' FUSE_PATCH='${FUSE_PATCH:-}'
export ALPINE_RELEASE='${ALPINE_RELEASE:-}' MINIROOTFS_SHA256='${MINIROOTFS_SHA256:-}'
export DOCKER_VERSION='${DOCKER_VERSION:-}' DOCKER_SHA256='${DOCKER_SHA256:-}'
export LINUX_VIRT_VERSION='${LINUX_VIRT_VERSION:-}' LINUX_SHA256='${LINUX_SHA256:-}'
sh '$PWD/guest/build.sh' '$PWD/out/agent/rekcod-agent' '$PWD/out/guest'
EOF
fi
echo "built: target/release/rekcod, out/guest ($(cat out/guest/VERSION 2>/dev/null || echo 'no guest'))"
