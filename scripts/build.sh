#!/usr/bin/env bash
# Build rekcod: host binary (signed for Virtualization.framework), guest agent,
# and the guest image (out/guest). The guest image needs a working docker
# engine; set REKCOD_BUILD_DOCKER_HOST to pick one (rekcod itself works).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p rekcod
cargo build --release -p rekcod-agent --target aarch64-unknown-linux-musl
codesign --force --sign - --entitlements rekcod.entitlements target/release/rekcod
mkdir -p out/agent && cp target/aarch64-unknown-linux-musl/release/rekcod-agent out/agent/

if [[ "${1:-}" != "--host-only" ]]; then
  DOCKER_HOST="${REKCOD_BUILD_DOCKER_HOST:-${DOCKER_HOST:-}}" docker build \
    --platform linux/arm64 \
    --build-context agent=out/agent \
    --target out --output type=local,dest=out/guest \
    guest
fi
echo "built: target/release/rekcod, out/guest ($(cat out/guest/VERSION 2>/dev/null || echo 'no guest'))"
