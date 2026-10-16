# Phase 3: installable rekcod (minus signing and brew)

Scope: shrink the guest, publish release artifacts, auto-fetch the guest, CI.
Not here: Developer ID signing/notarization (needs the user's Apple account) and
the Homebrew tap (skipped by request).

**Status (2026-10-16):** done, and since extended: releases bundle the dashboard, carry
SBOMs and a provenance attestation, and run all of CI first. Notarization is wired into
`release.yml` but off until the Apple secrets exist. The current pipeline is described in
[releasing.md](releasing.md); the sections below are the phase 3 design.

## Guest size

`root.img` is 584 MB, but holds 215 MB: the image is sized at 1.5× content
+ 256 MB of mostly-empty ext4. Changes:
- Size the image at content + 25% + 64 MB (logs and `/etc` writes only;
  all docker state lives on the data disk).
- Drop `ctr` (21 MB, debugging only).
- Ship it compressed: the release tarball (`gzip -9`) only stores real blocks.

## Release artifacts (GitHub Releases, tag `vX.Y.Z`)

| file | contents |
|---|---|
| `rekcod-X.Y.Z-darwin-arm64.tar.gz` | `bin/rekcod` (ad-hoc signed with the virtualization entitlement); since then also `share/rekcod/dashboard` |
| `rekcod-darwin-arm64.tar.gz` | the same, versionless, for `releases/latest/download` (added in 0.2.0) |
| `rekcod-guest-<GV>.tar.gz` | `Image`, `initramfs`, `root.img`, `VERSION` (`GV` = guest VERSION hash); the custom kernel has no `initramfs` |
| `SHA256SUMS` | sha256 of the tarballs |

The host binary is built with `REKCOD_GUEST_VERSION` and `REKCOD_GUEST_SHA256`
embedded (`option_env!`), so a binary knows the one guest it was released with.

## Auto-fetch

`rekcod start` with no local guest (no `$REKCOD_GUEST_DIR`, no `out/guest` next to a
dev build), but with an embedded guest version:
1. Download `rekcod-guest-<GV>.tar.gz` into `~/.rekcod/guest/<GV>/`. Use `curl`
   (always on macOS) from `$REKCOD_RELEASE_URL` (default: the GitHub release of the
   binary's version). If curl fails, fall back to `gh release download` (uses the
   user's gh auth). That fallback dates from when the repo was private; it's public
   now, and the fallback stays for networks where curl to GitHub fails.
2. Verify the sha256 against the embedded value; refuse to unpack on mismatch.
3. Unpack with the system `tar` into a temp dir, then rename into place (atomic).

Versioned directories mean a dev build never picks up a downloaded guest, and an
upgrade fetches the new guest beside the old one.

## CI (GitHub Actions)

As built in phase 3, and what it is now:

- `ci.yml` (push/PR): `macos-15` runs `cargo test --workspace`, clippy for host +
  agent, and builds the agent (cross, rust-lld). `ubuntu-24.04-arm` builds the
  guest image (smoke: the image builds and `root.img` is under 400 MB). The guest
  build was docker buildx then; it's `guest/build.sh` in plain chroots now
  ([guest-build.md](guest-build.md)). Since then CI also runs rustfmt, the agent's
  Linux-only tests, `cargo deny` and the dashboard checks.
- `release.yml` (tag `v*`): the arm Linux job builds the guest tarball; the macOS
  job builds the host with the embedded guest version and hash, packs, writes
  `SHA256SUMS`, and creates the release. It now runs all of `ci.yml` first and
  publishes only if that passes.
- e2e needs nested virtualization (hosted macOS runners can't). It runs locally, and
  `e2e.yml` runs it on a self-hosted Mac once one is registered.
