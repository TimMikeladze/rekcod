# RustFS in rekcod

**Status:** spec, 2026-10-12. Nothing here is built.

`platform.md` tracks 2 and 3 were specced around an S3 bucket (registry blobs, BuildKit
cache, `snap` backups) and shipped without one ("No registry, no S3" in "Tracks 2 and 3 as
built"). RustFS is the bucket. It's a single static Rust binary, S3-compatible, Apache-2.0,
so it runs as a LaunchAgent next to the controller the same way `controller serve` does.
No MinIO.

## Where it runs

`rekcod storage serve --listen 100.x.y.z:7710 --data ~/.rekcod/storage` downloads a
pinned RustFS release binary (checksum checked) into `~/.rekcod/bin/rustfs` and runs it.
`--install` makes it a LaunchAgent like the other fleet services, and
`scripts/spare-setup.sh` adds it. The controller learns the endpoint and root credentials
from `rekcod controller serve --storage http://...:7710` and keeps them in `fleet.db`.

One RustFS on one Mac to start. That Mac's disk is a single point of failure, which
`platform.md` says not to rely on. RustFS's distributed mode (4+ drives) is the upgrade path.
I haven't measured it on Macs.

## What uses it, in build order

1. **Per-app buckets.** `[storage] bucket = true` in `rekcod.toml` creates
   `app-<name>` plus an access key scoped to it by an IAM policy, and injects
   `S3_ENDPOINT`, `S3_BUCKET`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
   `AWS_REGION=us-east-1` into every instance. This gives stateless instances somewhere to
   put state, and it's the one users see. Previews get their own `app-<name>-pr-<n>`
   bucket, deleted with the preview. `app destroy` asks before deleting a non-empty bucket.
2. **Releases without rebuilds.** After a build, the node runs
   `docker save rekcod-app-<app>:<release> | zstd` into `rekcod-releases/<app>/<release>`.
   Other instances and rollbacks `docker load` it instead of cloning and building. This
   fixes "rollback is a full build" and the per-instance build. A real OCI registry
   (`rekcod registry` on `object_store`) can replace this later; the tarball is the dirty
   version with no new server.
3. **BuildKit cache.** `--cache-to type=s3,bucket=rekcod-cache,name=<app>` on the build, so
   a build on another node hits cache.
4. **Backups.** The controller uploads `fleet.db` (via SQLite `VACUUM INTO`, so no stop
   needed) every hour to `rekcod-backups/`, keeps 48. `rekcod snap save --push` uploads a
   snapshot's volumes so a pinned single-instance app can come back on another node.
5. **Local dev.** `rekcod storage up` runs `rustfs/rustfs` in the local VM on a free port
   (never kills whatever has 9000) and prints the env block. Menu bar gets it via a new
   `Kind` in `actions.rs`, per `CLAUDE.md`.

## Tenancy

Each app's key can only touch its own bucket. Root credentials stay on the controller and
the node ops that need them (2-4); they're never put in an instance's env. Keys are stored
like secrets today, in plain text in `fleet.db`, so this inherits that gap.

## CLI

```sh
rekcod storage serve|install|uninstall          # fleet
rekcod storage up|down|env                      # local VM
rekcod app storage NAME [ls|url KEY|rotate]     # bucket of an app
```

## Open questions

- Does RustFS's admin API take MinIO-style IAM policies today? I believe it does
  (`mc admin user add` / `policy attach` work against it) but it needs checking against
  the pinned version before step 1.
- Pin which release, and does its binary run unsigned under a LaunchAgent without a
  Gatekeeper prompt (we'd strip quarantine after the download)?

## Verification

`scripts/e2e-fleet.sh` gains: deploy an app with `bucket = true` that writes and reads an
object; a second app's key gets 403 on the first app's bucket; rollback with no `git clone`
in the node log; restore `fleet.db` from the latest backup.
