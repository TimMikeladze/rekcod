# Contributing

## Setup

- An Apple Silicon Mac. rekcod doesn't build or run anywhere else, except the guest agent,
  which cross-compiles to `aarch64-unknown-linux-musl`.
- Rust through `rustup`. `rust-toolchain.toml` pins the version, the musl target, clippy and
  rustfmt; rustup installs them on the first `cargo` command.
- Xcode command-line tools, for `codesign`. The binary needs the
  `com.apple.security.virtualization` entitlement (`rekcod.entitlements`) to boot a VM.
- Node 22 for the dashboard, and Bun for the website.

No Docker engine is needed. The guest image builds inside a running rekcod VM.

```sh
git config core.hooksPath .githooks   # once per clone: fmt, clippy and tests before each commit
scripts/build.sh --host-only          # target/release/rekcod (signed) and the agent
scripts/build.sh                      # also out/guest; needs `rekcod start` first
scripts/link.sh                       # once: ~/.local/bin/rekcod -> target/release/rekcod
```

`scripts/link.sh` links `~/.local/bin/rekcod` (or `DIR=...`) to `target/release/rekcod`, so every
later build is the `rekcod` on your PATH. It won't replace a real binary there, and
`--remove` takes the link away.

`target/release/rekcod` uses `out/guest` when it exists, through the link too. The guest's
downloads are pinned with a sha256 each in `guest/versions.env`; `guest/build.sh` builds it
with plain chroots on any arm64 Linux, `KERNEL=alpine` for Alpine's kernel instead of the
custom one.

Don't re-sign `target/release/rekcod` in place while a VM runs from it: macOS kills the
daemon. `scripts/build.sh` and `scripts/package.sh` sign a copy and move it in.

## Tests

What CI runs (`.github/workflows/ci.yml`):

```sh
cargo fmt --all --check
cargo clippy --locked -p rekcod -p rekcod-proto -- -D warnings
cargo clippy --locked -p rekcod-agent --target aarch64-unknown-linux-musl -- -D warnings
cargo test --workspace --locked
cargo deny --locked check
python3 scripts/check-docs.py
(cd dashboard && npm ci && npm audit --omit=dev --audit-level=high && npm test && npm run typecheck && npm run lint && npm run test:e2e)
(cd website && bun install --frozen-lockfile && bun test && bun run typecheck && bun run build && bun run check:links)
```

The host job also runs the VM-free scripts: `scripts/e2e-edges.py`, the stand-in-ssh path of
`scripts/e2e-engines.sh`, `scripts/test-install.sh`, `packaging/server/test.sh` and
`.github/scripts/check_release_jobs.py`. The pre-commit hook runs fmt, clippy and the Rust
tests, plus the dashboard checks when a staged file is under `dashboard/`. The agent's
Linux-only tests run in CI on arm64 Linux; locally, `rekcod x cargo test -p rekcod-agent`.

`scripts/check-docs.py` fails on a reference to a `docs/*.md` file that doesn't exist, a
README section named in a comment or linked by anchor that doesn't exist, a broken relative
link in a Markdown file, or a missing `SECURITY.md`, `CONTRIBUTING.md` or CHANGELOG entry for
`Cargo.toml`'s version. Point code comments at the README by section title, as
`(README.md, "Network and secrets")`.
`tests::every_command_is_in_the_readme` fails when a command `--help` lists isn't in the
README's command reference.

### The VM suites

The VM e2e scripts live in `scripts/` and boot real VMs. `scripts/e2e.sh` starts and finally
stops the main VM, and stopping deletes running `--rm` containers. Run `docker ps` first;
the script refuses the stop while containers it didn't start are running, unless
`E2E_ALLOW_STOP=1`. `scripts/e2e-sandbox.sh`, `e2e-fleet.sh`, `e2e-storage.sh`,
`e2e-lifecycle.sh`, `e2e-engines.sh` and `e2e-macos.sh` use their own homes and leave the
main VM alone.

`.github/workflows/e2e.yml` runs all of them on a self-hosted Mac with the label
`rekcod-e2e`, a logged-in user and `REKCOD_HOME=$HOME/.rke2e`, deleted before and after each
run. Its jobs are skipped until the repository variable `REKCOD_E2E_RUNNER` is `true`.

The repository is public, so that runner must **never** run pull request code. Don't add
`pull_request`, `pull_request_target` or `push` triggers to `e2e.yml`; it runs from
`workflow_dispatch`, from `release.yml` on a tag and from `bump.yml`, all of which need write
access. Keep "Require approval for all outside collaborators" on under Settings, Actions.

## The CLI, menu bar and dashboard stay in sync

Every user-facing action is a CLI command first. The menu bar (`rekcod menubar`) and the web
dashboard only render the action lists in `crates/rekcod/src/actions.rs` and run the
action's `rekcod` command line; the dashboard gets them from `rekcod state`, and another
Mac's engine from `rekcod host state`, whose actions run through `rekcod host exec`
([docs/engines-and-objects.md](docs/engines-and-objects.md)).

When you add a CLI feature a user acts on, add a `Kind` and put it in the right list in the
same change, then update the README and `dashboard/fixtures/state.json`. An action that needs
a value declares an `Input` and puts `{key}` in its argv; its checks live in
`actions::invalid` and `dashboard/src/lib/inputs.ts`, which change together. The parity tests
fail otherwise: `actions::tests::every_kind_is_offered`,
`menubar::tests::menu_offers_every_action_kind`, `tests::every_action_is_a_cli_command` and
`state::tests::state_offers_every_action_kind`.

## Spec first

For anything that touches several files, write a short design doc in `docs/` first: what
it does, the approach, the pieces, what you measured. Then build it, then verify it with
real runs and say what you couldn't verify. Small fixes don't need one. User-facing
behaviour goes in the README; `docs/` holds plans and audits.

## Commits and changelog

Commit subjects follow the existing log: `feat(sandbox): ...`, `fix: ...`, `ci: ...`,
`docs: ...`, `refactor(fleet)!: ...` for a breaking change. The body says what changed
and any number you measured. Add user-visible changes to `CHANGELOG.md` under the
`Unreleased` version, and breaking ones under "Upgrading from" with the steps.

Docs and comments state facts about the code: what a command does, a default, what happens
on failure. A comment says what the code can't (why, a gotcha), or nothing.

## Releasing

Releases are cut by the maintainer from `main`.

1. Set `version` in `Cargo.toml` (the workspace version) and run `cargo check` so
   `Cargo.lock` follows.
2. In `CHANGELOG.md`, replace `Unreleased` in that version's heading with the date
   (`## [0.3.0] - 2026-11-20`) and add its compare link at the bottom.
3. Commit to `main`, push, then tag and push the tag:

   ```sh
   git tag v0.3.0 && git push origin v0.3.0
   ```

4. Add a new `## [X.Y.Z] - Unreleased` section for the next version.

`release.yml` runs these jobs, and publishes nothing when one fails:

- `tag`: the tag is on `main`, matches `Cargo.toml`, and `CHANGELOG.md` has a section for
  it (`scripts/release-notes.py`), which becomes the release notes.
- `ci`: all of `ci.yml`, including the guest build and its QEMU boot.
- `e2e`: `e2e.yml` with that guest on the self-hosted Mac. While `REKCOD_E2E_RUNNER` isn't
  `true` it's skipped, and a skip blocks the release unless the repository variable
  `REKCOD_RELEASE_WITHOUT_E2E` is `true`; the release notes then say the VM suites didn't run.
  Run the VM suites by hand before tagging in that case.
- `build`: the binary, guest tarball, bundled dashboard and CycloneDX SBOMs, with no secrets.
- `sign`: Developer ID signing and notarization in the `release` environment when all five
  secrets are set, ad-hoc signing otherwise; then the tarballs and `SHA256SUMS`.
- `publish`: a build provenance attestation for every file, `rekcod.sigstore.json` as an
  asset, and `gh release create`.

The signing secrets live in the `release` environment (Settings, Environments), limited to
`v*` tags:

| Secret | Value |
| --- | --- |
| `APPLE_DEVELOPER_ID_P12` | the "Developer ID Application" certificate and key, exported as `.p12`, then base64 |
| `APPLE_DEVELOPER_ID_P12_PASSWORD` | the password set on that export |
| `APPLE_API_KEY_P8` | an App Store Connect API key: the text of `AuthKey_<id>.p8`, not base64 |
| `APPLE_API_KEY_ID` | that key's ID |
| `APPLE_API_ISSUER_ID` | the issuer ID shown above the keys list |

```sh
base64 -i DeveloperID.p12 | gh secret set APPLE_DEVELOPER_ID_P12 --env release
gh secret set APPLE_DEVELOPER_ID_P12_PASSWORD --env release
gh secret set APPLE_API_KEY_P8 --env release < AuthKey_ABC123DEFG.p8
gh secret set APPLE_API_KEY_ID --env release --body ABC123DEFG
gh secret set APPLE_API_ISSUER_ID --env release --body 00000000-0000-0000-0000-000000000000
```

Without them every release is ad-hoc signed and not notarized. To check a published release:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
gh attestation verify rekcod-darwin-arm64.tar.gz -R TimMikeladze/rekcod
```

Release tarballs carry `THIRD-PARTY-NOTICES.txt`; the guest tarball also has the kernel
config and `packages.txt`. `bump.yml` moves the guest and Mac CLI pins to the newest releases
weekly and opens the `bump/versions` PR. `audit.yml` runs `cargo deny`, `npm audit` and
`bun audit` weekly on `main` and builds the guest twice to compare the results.

Security issues go through [SECURITY.md](SECURITY.md), not public issues.
