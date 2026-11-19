# rekcod reset

`rekcod reset` gives the local Docker engine a new, empty data disk without uninstalling
rekcod. reset.rs is built like `uninstall` (lifecycle.rs): `facts` reads the machine, `check`
holds the refusals, `plan` turns facts into `Step`s (pure, unit-tested), `execute` runs them
with ok/FAIL lines. User-facing behaviour is in README.md, "Reset the engine".

## What goes

- `vm/data.img`: images, containers, volumes, networks, build cache.
- With `--all`: agent sandboxes (`sb/`, running ones stopped first, then `sandbox rm --all`
  and the leftovers of failed creates), golden disks (`golden/`), local snapshots.

The `x --sandbox` VM's disk (`sandbox/`) isn't touched; `cli::stop` only stops that VM.

## Snapshots

Snapshots are volumes on data.img (snap.rs), so keeping them means copying them out and
back. Without `--all`, while the VM runs: `SaveSnapshots` (`snap::carry_out`) writes every
volume labelled `dev.rekcod.snap` (markers and old generations too) to
`vm/reset-snapshots/<name>.tar`, the helper image to `helper.tar` (it's on the disk being
deleted) and `manifest.json` last, with labels and SHA-256s. `RestoreSnapshots`
(`snap::carry_in`) checks each archive, recreates the volume with its labels, unpacks it and
deletes the directory. `check` refuses when the Mac lacks the snapshots' size plus 10%.
Snapshots pushed to an outside S3 are never touched; `storage up`'s RustFS is the
`rekcod-storage` volume on data.img, so what was pushed there goes, and the prompt says so.

- VM stopped: reset boots it (`--no-switch`) before the prompt, reads the facts again and
  plans as for `--no-start`, so it ends stopped; the context isn't touched. Declining the
  prompt stops it again. If that boot fails, the prompt says the snapshots can't be copied
  out, and a `yes` resets without them (a disk that doesn't boot is often why you reset).
- `--no-start`: boots once for the import, then stops.
- The boot fails: the archives stay. A later `rekcod reset` sees `manifest.json` and only
  boots and imports (`Facts::unfinished`), deleting nothing.
- The disk wasn't deleted (stop or delete failed): the copies are dropped, the snapshots are
  still on the old disk.

## data.fresh

`ResetDataDisk` holds `run/boot.lock`, checks no daemon runs, deletes data.img, then writes
`vm/data.fresh`: the state `prepare_vm` leaves on a first install. The next boot passes
`rekcod.fresh=1`, data-disk.sh formats the blank disk, and `boot`/`forget_fresh_disk` drop
the marker once dockerd answers. root.img, guest-version, mac-address and warm-paths stay.

## Order and failures

`SaveSnapshots` → `StopVm` → `RemoveSandboxes` → `RemoveGolden`… → `ResetDataDisk` →
`Start` → `RestoreSnapshots` (→ `StopVm` for `--no-start` with snapshots).

- A failed `SaveSnapshots` or `StopVm` skips everything after it: nothing is deleted.
- A failed `Start` skips `RestoreSnapshots`. Other failures are reported and the rest runs.
  Any failure exits non-zero.
- `Start` is `cli::start` with `--no-switch` and the saved disk size, so config.json and the
  docker context stay as they were. `StopVm` switches the context back only when the VM ends
  up stopped on the default home.

## Refusals

- A fleet node or controller of this home runs: its launchd job (`installed_here` and
  `launchctl print` shows a pid), the controller's `fleet.lock` beside its `--db`, or
  `run/node.lock`, which `node serve` now holds while it runs.
- No `--yes` and stdin isn't a terminal.

## Front ends

`Kind::Reset` (`reset --yes`) and `Kind::ResetAll` (`reset --all --yes`) in `vm_actions`,
both with a confirm and interactive. The dashboard opens them in Terminal rather than
streaming them: a stream ends with its sheet, a reset must not stop half way.
