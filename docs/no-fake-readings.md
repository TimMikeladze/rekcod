# No fake readings

An audit (2026-10-16) found no mock or demo data in the CLI, the menu bar or the dashboard.
Fixture data (`dashboard/fixtures/state.json`, `src/lib/server/fixtures.ts`, `fakeNodes`,
`fakeSample`, `fakeFs`) only runs with `REKCOD_STATE_FILE`, which only Playwright sets; Rust
sample data is all `#[cfg(test)]`.

What did reach users: values we couldn't read, turned into `0`, `null` or an empty list and then
shown as if measured. The rule from here on:

- An unknown value is `null` on the wire and `–` (or left out) on screen. Never `0`, never a
  full or empty bar.
- A failed read says it failed. An empty list means we asked and got nothing back.
- Charts don't draw lines across time they have no samples for.

## Rust

- Guest agent: `statvfs` failure sends `disk_used`/`disk_size` as `null`.
- Node heartbeat: `cpus`, `load`, disk and `mem_free_mib` sent as `null` when unreadable. The
  controller stores NULL (migration 5 rebuilds `nodes` with nullable columns and turns old
  `cpus = 0`/`disk_total = 0` rows into NULL). A missing `mem_free_mib` keeps the last value,
  since scheduling filters on it. `node ls` prints the JSON, so unknown is `null`.
- `actions::age` takes `Option<u64>`; missing `last_seen`/`created` print `?`, not "20000d ago".
- `rekcod status` and the menu bar header skip CPUs/memory when the status file lacks them.
- Menu bar: before the first model loads it shows "Loading…", not an empty running VM.
  Controller reads behind it and `rekcod state` get 10 s (`actions::STATE_READ`), not 900 s.
- Docker list calls that fail while the VM runs are reported in `local_errors` instead of
  looking like "No containers".

## Dashboard

- `docker stats` `--` parses to `null`; tables and tiles show `–`.
- `Meter` takes `number | null`; `null` renders a muted bar.
- Node memory/load/disk, VM memory, sandbox RSS and disk usage guard on `null`.
- `issues.ts` only flags low memory when `mem_free_mib` is known.
- Traffic charts fill idle minutes with zero requests and break latency lines there.
- File-sharing rate uses the server's read time so cached polls don't make fake dips.
- Line charts break where samples are more than two intervals apart.
