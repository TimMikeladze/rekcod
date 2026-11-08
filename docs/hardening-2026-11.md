# Hardening after the 2026-11-07 audit

Fixes for the 39 findings in [audit-2026-11-07.md](audit-2026-11-07.md). Every fix
ships with a regression test for the case the audit reproduced. Status per finding
is tracked in the audit's Fix status section.

## Work split

The work is split by file so the pieces merge without conflicts.

| Piece | Findings | Files |
| --- | --- | --- |
| Dashboard | S1, R4, V1 (npm) | `dashboard/` |
| Sandbox copy and API | S2, S3, S4, S6, S7, C1, C2 | `sandbox.rs`, `sandbox_api.rs`, `safefs.rs` |
| Fleet apps | F1, F2, F4, F5, F6, F7 | `fleet.rs` (apps, previews, quota, destroy) |
| Fleet jobs and nodes | J1 to J4, F3, D9, S5, DOC1 | `fleet.rs` (jobs, registration, restore), `xrun.rs` |
| Snapshots and S3 | D1, D2, D5, D6, D7, D8, D10, F7 (`s3.rs`) | `snap.rs`, `s3.rs`, `cli.rs` import |
| Runtime | D3, R1, R2, R3, D4, O1, O2, O3, V1 (cargo) | `vm.rs`, `proxy.rs`, `daemon.rs`, `boot.sh`, `lifecycle.rs`, `edge/`, `paths.rs` |

## Decisions

Dashboard auth (S1) drops cookies. `/auth?token=` stores the token in
`localStorage`, which is scoped to scheme, host and port, and every API call sends
`Authorization: Bearer`. Page shells carry no state and stay public; streams use
`fetch` instead of `EventSource`; images are fetched as blobs. The old cookie is
expired on first load.

Sandbox authorization (S2, S3) resolves the target sandbox before every operation
and checks its project and capabilities against the server's policy. Listing is
filtered. Node-only routes (`/instances`, `/jobs`) exist only when the server runs
as a fleet node.

Sandbox apply (S4, S6, S7, C1, C2) runs under a per-sandbox lock shared with diff.
Protected names compare case- and normalization-insensitively. Link targets are
resolved against the final tree. The manifest records mode.

Bucket names (F1) keep their current spelling so existing data stays put. The
literal and hashed domains are made disjoint by validation, and the controller
refuses a bucket another app already owns.

Destroy (F6, F7) takes the app lock, bumps the app's generation, and keeps the app
row in `destroying` until storage cleanup succeeds; the reconcile loop retries it.

Jobs (J1 to J4) are stored as `pending` with their quota before dispatch, starts
are idempotent by ID, `lost` means unknown rather than terminal, and cancel stays
`cancelling` until the worker confirms.

Snapshots (D1, D2, D6) publish archives under a generation ID and the manifest
last; `finish` commits the generation it was asked to, not the newest timestamp;
markers live in their own name domain.

## Follow-ups

The elastic-memory remap failure is handled on the host: a chunk that was unmapped
and couldn't be mapped back is quarantined and the release batch is refused, so the
guest keeps it released. It's tested with a fake region; no VM fault injection ran.

The dashboard watcher stop race is closed by `REKCOD_NO_BOOT=1`, which the `events`
and `stats` followers set: passthrough then exits instead of booting a stopped VM.
