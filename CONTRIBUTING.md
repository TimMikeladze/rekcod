# Contributing

## Setup

- An Apple Silicon Mac. rekcod doesn't build or run anywhere else, except the guest agent,
  which cross-compiles to `aarch64-unknown-linux-musl`.
- Rust through `rustup`. `rust-toolchain.toml` pins the version, the musl target, clippy and
  rustfmt; rustup installs them on the first `cargo` command.
- Xcode command-line tools, for `codesign`. The binary needs the
  `com.apple.security.virtualization` entitlement (`rekcod.entitlements`) to boot a VM.
- Node 22, for the dashboard only.

No Docker engine is needed. The guest image builds inside a running rekcod VM.

```sh
git config core.hooksPath .githooks   # once per clone: fmt, clippy and tests before each commit
scripts/build.sh --host-only          # target/release/rekcod (signed) and the agent
scripts/build.sh                      # also out/guest; needs `rekcod start` first
```

`target/release/rekcod` uses `out/guest` when it exists. Guest builds are described in
[docs/guest-build.md](docs/guest-build.md); its downloads are pinned in `guest/versions.env`.

Don't re-sign `target/release/rekcod` in place while a VM runs from it: macOS kills the
daemon. `scripts/build.sh` and `scripts/package.sh` sign a copy and move it in.

## Tests

What CI runs (`.github/workflows/ci.yml`):

```sh
cargo fmt --all --check
cargo clippy --locked -p rekcod -p rekcod-proto -- -D warnings
cargo clippy --locked -p rekcod-agent --target aarch64-unknown-linux-musl -- -D warnings
cargo test --workspace --locked
cargo deny check
(cd dashboard && npm ci && npm audit --omit=dev --audit-level=high && npm test && npm run typecheck && npm run lint && npm run test:e2e)
```

The agent's Linux-only tests run in CI on arm64 Linux. Locally: `rekcod x cargo test -p rekcod-agent`.

The e2e scripts need a running VM and are listed in README's Test section. `scripts/e2e.sh`
starts and finally stops the main VM, and stopping deletes running `--rm` containers.
Check `docker ps` first; the script refuses the stop while containers it didn't start are
running, unless `E2E_ALLOW_STOP=1`. `scripts/e2e-sandbox.sh`, `e2e-fleet.sh` and
`e2e-storage.sh` use their own homes and leave the main VM alone.

## The CLI and the menu bar stay in sync

Every user-facing action is a CLI command first. The menu bar (`rekcod menubar`) and the web
dashboard only render the action lists in `crates/rekcod/src/actions.rs` and run the
action's `rekcod` command line; the dashboard gets them from `rekcod state`.

When you add a CLI feature a user acts on, add a `Kind` and put it in the right list in the
same change, then update [docs/frontends.md](docs/frontends.md) and the README. The parity
tests fail otherwise: `actions::tests::every_kind_is_offered`,
`menubar::tests::menu_offers_every_action_kind`, `tests::every_action_is_a_cli_command` and
`state::tests::state_offers_every_action_kind`.

## Spec first

For anything that touches several files, write a short design doc in `docs/` first: what
it does, the approach, the pieces, what you measured. Then build it, then verify it with
real runs and say what you couldn't verify. The existing `docs/phase*.md`, `docs/sandbox.md`
and `docs/platform.md` show the shape. Small fixes don't need one.

## Commits and changelog

Commit subjects follow the existing log: `feat(sandbox): ...`, `fix: ...`, `ci: ...`,
`docs: ...`, `refactor(fleet)!: ...` for a breaking change. The body says what changed
and any number you measured. Add user-visible changes to `CHANGELOG.md` under
`Unreleased`.

Docs and comments state facts about the code: what a command does, a default, what happens
on failure. A comment says what the code can't (why, a gotcha), or nothing.

Releases are cut by the maintainer ([docs/releasing.md](docs/releasing.md)). Security
issues go through [SECURITY.md](SECURITY.md), not public issues.
