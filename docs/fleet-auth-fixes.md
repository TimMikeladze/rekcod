# Fleet auth fixes (audit 2026-11-18)

Fixes for the fleet-auth findings in `docs/audit-2026-11-18.md` that land on the
`audit-fixes-auth` branch. Each item names the finding, the decision and where it lives.

## Jobs and env

- FA-2: `sandbox_api::check_env` requires `KEY=VALUE` with a shell-style key. The
  controller refuses a bad job or schedule spec at submit (`start_job`, schedule create);
  the node refuses it again before it makes the job's sandbox. Fleet jobs also run the
  docker CLI with a cleared environment (`ExecOptions::clean_env`): PATH, HOME and the
  variables rekcod set on the command itself. Local `sandbox exec --secret` keeps inheriting.
- FA-10, FD-21: `docker run` for an app instance reads values from a 0600 `--env-file` in
  the sandbox home, removed after the call. A value with a line break (a PEM key) can't go in
  an env file; it goes in the docker client's environment with `-e KEY`, except for names
  the client or the macOS loader act on (PATH, `DOCKER_*`, `DYLD_*`, `LD_*`, ...), which are
  refused. The alert webhook URL goes to curl on stdin (`-K -`).

## Tokens, schedules, namespaces

- FA-4: token and member names are 1 to 64 of `[A-Za-z0-9._@-]`. `read_head` refuses a
  request line that isn't three parts, path segments are percent-decoded, and the CLI
  encodes names. `DELETE /tokens` with `{"name"}` in the body removes rows with older names.
- FA-11: `max_sandboxes` is -1 or 0 to 100000, read as i64. A migration clamps stored values.
  Watchdog duties run under `catch_unwind`.
- FA-8, FD-17: revoking a name, or removing a member, deletes their schedules (in that
  namespace for a member) and cancels their active jobs. `run_schedules` skips a schedule
  whose owner has no unexpired token or no member role.
- FA-13: `held_by(owner)` counts sandboxes that aren't app instances plus active jobs; used
  by create, fork and `start_job`.
- FA-14: deleting a namespace deletes its finished jobs, dead sandbox rows, instance logs and
  traffic, and moves audit rows to `NAME (deleted TS)`. Creating a namespace refuses a name
  that still has job or sandbox rows.
- FA-17: `GET /tokens` (admin) and `GET /audit` (admin, or a namespace owner for their
  namespace), as `rekcod controller tokens` and `rekcod controller audit`. Denied roles, bad
  tokens (one row per address per minute), quota, pool, env, secrets and schedule changes
  are recorded; destroy is recorded with its outcome.
- FA-12, FD-15: `Controller::db()` at the sites that unwrapped the lock; background duties
  take app locks with `try_app_lock`. CI fails on a new `.lock().unwrap()` in fleet.rs.

## Webhooks

- FA-7: each app has its own webhook secret, sealed in its row and shown by
  `rekcod app webhook NAME`. GitHub posts to `/webhooks/github/NS/APP`; the event's
  repository must be that app's `git_url`. The fleet-wide secret and `/webhooks/github`
  are gone. The PR comment URL is built from `repository.full_name` against
  `GITHUB_API_URL` (default `https://api.github.com`), with `--proto =https -m 10`.
- FA-9: a fork's pull request deploys only on a `labeled` event for `rekcod-preview`. With
  `GITHUB_TOKEN` the label is removed after the deploy, so each new head needs it again.
- FA-6, FA-15: a delivery is recorded by the SHA-256 of its signed body, after it succeeds,
  under the preview's lock. `X-GitHub-Delivery` is required. `pr_seen` keeps the newest
  `pull_request.updated_at` per base app and PR, and outlives the preview; older events are
  ignored.

## Connections and quota lock (FD-09, FA-3, RD-04)

- Sandbox create and fork reserve a `creating` row under the namespace quota lock, then call
  the node without it. The row is counted by quotas, hidden from listings and removed by
  `sweep`.
- The controller gives TLS handshake plus request head 10 s, sets a write timeout, caps
  connections per peer address (loopback exempt), and keeps a reserve of slots for addresses
  that recently authenticated as a node or edge.
- Left to the deploy branch (`audit-fixes-deploy`), which already changes that code: git's
  low-speed limits and deadline in `git_cmd`/`resolve`, and the 409 for a third queued deploy
  of one app.
- `proxy()` connects to a node within 10 s, with a 60 s write timeout and the 900 s read
  timeout `call` uses (none for `?follow` streams).
