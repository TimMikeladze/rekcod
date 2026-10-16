# Releasing

How a tag becomes a GitHub release, what's signed and attested, and the two pieces that
stay off until someone turns them on: Developer ID notarization and the self-hosted e2e
runner.

## Before 0.3.0 ships (state on 2026-10-16)

`main` is at 0.3.0 and pushed; CI is green. These were left for the owner on purpose and
each one is a single step:

1. **Tag the release:** `git tag v0.3.0 && git push origin v0.3.0`. release.yml runs the
   tests, builds the guest and the bundled dashboard, and publishes the GitHub release.
   Until then `releases/latest` (what `scripts/install.sh` and `rekcod upgrade` fetch) is
   0.2.1.
2. **Notarize:** add the five secrets in "Developer ID signing and notarization" below. The
   next tag is then signed with the Developer ID and notarized; without them it's ad-hoc
   signed and users strip the quarantine attribute (the README says how).
3. **e2e on tags:** register a Mac as described in "Self-hosted e2e runner" and set
   `REKCOD_E2E_RUNNER=true`. Until then run `scripts/e2e.sh`, `scripts/e2e-sandbox.sh`,
   `scripts/e2e-fleet.sh` and `scripts/e2e-lifecycle.sh` by hand before tagging.
4. **Homebrew:** `gh repo create TimMikeladze/homebrew-tap --public`, copy
   `packaging/homebrew/rekcod.rb` to `Formula/rekcod.rb` there with `version` and `sha256`
   from the release's `SHA256SUMS`. The tap has to be public for `brew install` without a
   token.
5. **Vulnerability reports:** turn on private vulnerability reporting
   (`gh api -X PUT repos/TimMikeladze/rekcod/private-vulnerability-reporting`), which
   `SECURITY.md` points to.
6. **Outside security review:** hand `docs/security-review.md` to a reviewer. An internal
   adversarial pass has been done (`docs/production-readiness.md`, "Status").

## Cutting a release

1. Bump `version` in `Cargo.toml` (the workspace version) and run `cargo check` so
   `Cargo.lock` follows.
2. Move the `Unreleased` entries in `CHANGELOG.md` under the new version.
3. Commit, then tag and push:

```sh
git tag v0.3.0 && git push origin v0.3.0
```

`release.yml` refuses a tag that doesn't match `Cargo.toml`.

## What release.yml does

1. `ci`: the whole of `ci.yml` as a reusable workflow. That's fmt, clippy (host and agent),
   `cargo test --workspace --locked` on macOS, the agent's Linux-only tests on arm64 Linux,
   `cargo deny check`, the dashboard's audit, unit, type, lint and Playwright checks, and
   the guest image build. The guest is uploaded as the `guest` artifact.
2. `e2e`: `e2e.yml` on the self-hosted Mac with that guest. Skipped while
   `REKCOD_E2E_RUNNER` isn't `true` ([below](#self-hosted-e2e-runner)), and a skip doesn't
   block the release. A failure does.
3. `release` (macOS, the only job with write permissions):
   - `scripts/package.sh --with-dashboard`: host binary with the guest version and sha256
     embedded, the dashboard's standalone build in `share/rekcod/dashboard`, the guest
     tarball and `SHA256SUMS`.
   - Developer ID signing and notarization if the secrets exist, ad-hoc signing otherwise.
   - CycloneDX SBOMs for the host binary, the guest agent and the dashboard.
   - A build provenance attestation (Sigstore, via `actions/attest-build-provenance`) for
     every file in `out/release`.
   - `gh release create` with generated notes.

Release assets:

| file | what |
|---|---|
| `rekcod-X.Y.Z-darwin-arm64.tar.gz` | `bin/rekcod`, `share/rekcod/dashboard` |
| `rekcod-darwin-arm64.tar.gz` | the same, for `releases/latest/download` and `scripts/install.sh` |
| `rekcod-guest-<GV>.tar.gz` | `Image`, `root.img`, `VERSION`; `rekcod start` downloads it |
| `SHA256SUMS` | sha256 of the three tarballs |
| `rekcod-X.Y.Z-darwin-arm64.cdx.json` | SBOM: crates in the host binary |
| `rekcod-agent-X.Y.Z-linux-arm64.cdx.json` | SBOM: crates in the guest agent |
| `rekcod-dashboard-X.Y.Z.cdx.json` | SBOM: npm packages in the dashboard (dev dependencies left out) |

The guest's own packages (Alpine, dockerd, the kernel) aren't in an SBOM yet. Their
versions and hashes are pinned in `guest/versions.env`.

## Verifying a release

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
gh attestation verify rekcod-darwin-arm64.tar.gz -R TimMikeladze/rekcod
```

## Developer ID signing and notarization

Off until all five repository secrets exist. With any of them missing, the release job
skips both steps and ships the ad-hoc signed binary, as every release so far has.

| secret | value |
|---|---|
| `APPLE_DEVELOPER_ID_P12` | the "Developer ID Application" certificate and its private key, exported from Keychain Access as `.p12`, then base64 |
| `APPLE_DEVELOPER_ID_P12_PASSWORD` | the password set on that export |
| `APPLE_API_KEY_P8` | an App Store Connect API key (Users and Access, Integrations, Team Keys; Developer role is enough): the text of `AuthKey_<id>.p8`, not base64 |
| `APPLE_API_KEY_ID` | that key's ID |
| `APPLE_API_ISSUER_ID` | the issuer ID shown above the keys list |

```sh
base64 -i DeveloperID.p12 | gh secret set APPLE_DEVELOPER_ID_P12
gh secret set APPLE_DEVELOPER_ID_P12_PASSWORD
gh secret set APPLE_API_KEY_P8 < AuthKey_ABC123DEFG.p8
gh secret set APPLE_API_KEY_ID --body ABC123DEFG
gh secret set APPLE_API_ISSUER_ID --body 00000000-0000-0000-0000-000000000000
```

With them set, the release job:

1. Imports the `.p12` into a temporary keychain and finds the Developer ID identity.
2. Runs `package.sh` with `CODESIGN_IDENTITY` set, so the binary is signed with that
   identity, `--options runtime` (hardened runtime), `--timestamp` and
   `rekcod.entitlements` (`com.apple.security.virtualization`).
3. Zips the binary from the tarball and runs `xcrun notarytool submit --wait`. Anything but
   `Accepted` prints the notary log and fails the release.
4. Deletes the keychain.

A bare Mach-O can't be stapled, so the ticket lives with Apple and Gatekeeper fetches it on
first launch. `curl` doesn't set the quarantine attribute, so `scripts/install.sh` installs
are never checked by Gatekeeper at all. Notarization matters for tarballs downloaded with
a browser.

I haven't run this path: there's no Developer ID yet. Things I'd expect to need attention
on the first notarized release: whether the hardened runtime needs any entitlement beyond
`com.apple.security.virtualization` (VM boot, Rosetta, the vsock bridges), and whether
`notarytool` flags anything in the binary. To try it before a real release, set the secrets
on a fork, push a tag there that matches its `Cargo.toml`, and delete the fork's release
afterwards. Once a notarized release is out, update README's Install note and Limitations
("not notarized").

## Self-hosted e2e runner

`e2e.yml` runs `scripts/e2e.sh` (which runs `scripts/e2e-sandbox.sh` before its final
stop) and `scripts/e2e-fleet.sh` on a Mac. Hosted macOS runners can't nest VMs, so this
needs real hardware. Every job in it is skipped until the variable is set, so nothing queues
before a runner exists.

The Mac:

- Apple Silicon, macOS 15 or later, not itself a VM.
- A dedicated user account, logged in. rekcod has only been tested from a user session, so
  I'd keep the runner in one. Nothing else should use rekcod as that user: the suite points
  the `rekcod` docker context at its own VM.
- Xcode command-line tools (`codesign`), `rustup`, `git`, `python3`, `sqlite3`, and the
  docker CLI with compose and buildx (`brew install docker docker-compose docker-buildx`).
- About 30 GB free for images and the fleet's VMs.

Register it:

1. Repository Settings, Actions, Runners, New self-hosted runner, macOS ARM64. Follow the
   download and `./config.sh` steps, and add the label `rekcod-e2e` when asked.
2. `./svc.sh install && ./svc.sh start` runs it as a LaunchAgent for that user.
3. Turn the workflow on:

```sh
gh variable set REKCOD_E2E_RUNNER --body true
```

The job uses `REKCOD_HOME=$HOME/.rke2e`, deleted before and after each run, so it never
touches the runner user's `~/.rekcod`. The fleet suite uses `~/.rkfc`, `~/.rkf1` and
`~/.rkf2`. One run at a time (`concurrency: rekcod-e2e`).

The repo is public, so this runner must never run pull request code: `e2e.yml` only
triggers from `workflow_dispatch` and from `release.yml` on a tag, both of which need write
access. Keep it that way, and keep "Require approval for all outside collaborators" on
under Settings, Actions.

To turn it off again: `gh variable delete REKCOD_E2E_RUNNER`.

## Dependency checks

- `ci.yml`'s `deny` job runs `cargo deny --locked check` against `deny.toml`:
  RustSec advisories, licenses that can ship in an Apache-2.0 binary, duplicate crates
  (warnings only), crates.io as the only source.
- The dashboard job runs `npm audit --omit=dev --audit-level=high`. Only production
  dependencies count, because only those ship in the standalone bundle.
- Dependabot (`.github/dependabot.yml`) opens weekly grouped PRs for cargo, the dashboard's
  npm packages and the pinned actions.
- Actions are pinned by commit SHA with the tag in a comment. Dependabot bumps both.
- `rust-toolchain.toml` pins the compiler. Bump it by hand.
- `guest/versions.env` pins the guest's downloads ([guest-build.md](guest-build.md)).
