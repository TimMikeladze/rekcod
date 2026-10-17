#!/usr/bin/env bash
# out/server/rekcod-server-<version>.pkg: double-click on a Mac to make it a one-machine
# fleet (docs/server-pkg.md). Runs scripts/package.sh first, so it needs out/guest.
#
# SERVER_DOMAIN=apps.example.com   apps are served at <app>.<domain> (default apps.localhost)
# INSTALLER_IDENTITY="Developer ID Installer: ..."   sign the pkg; unsigned otherwise
# SKIP_PACKAGE=1                  reuse out/release from an earlier package.sh
set -euo pipefail
cd "$(dirname "$0")/.."
guest=${GUEST_DIR:-out/guest}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[[ ${SKIP_PACKAGE:-} == 1 ]] || scripts/package.sh
out=out/server
rm -rf "$out" && mkdir -p "$out/root/usr/local/share/rekcod/guest"
tar -xzf "out/release/rekcod-$version-darwin-arm64.tar.gz" -C "$out/root/usr/local" bin
cp "$guest/Image" "$guest/root.img" "$guest/VERSION" "$out/root/usr/local/share/rekcod/guest/"
[[ -f $guest/initramfs ]] && cp "$guest/initramfs" "$out/root/usr/local/share/rekcod/guest/"
echo "${SERVER_DOMAIN:-apps.localhost}" > "$out/root/usr/local/share/rekcod/server-domain"

pkgbuild --quiet --root "$out/root" --scripts packaging/server/scripts \
  --identifier dev.rekcod.server --version "$version" --install-location / --ownership recommended \
  "$out/rekcod-server-component.pkg"
sed "s/@VERSION@/$version/" packaging/server/distribution.xml > "$out/distribution.xml"
pkg=$out/rekcod-server-$version.pkg
sign=()
[[ -n ${INSTALLER_IDENTITY:-} ]] && sign=(--sign "$INSTALLER_IDENTITY" --timestamp)
productbuild --quiet --distribution "$out/distribution.xml" --resources packaging/server \
  --package-path "$out" ${sign[@]+"${sign[@]}"} "$pkg"
rm -rf "$out/root" "$out/rekcod-server-component.pkg" "$out/distribution.xml"
ls -l "$pkg"
