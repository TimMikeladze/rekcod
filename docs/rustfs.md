# RustFS in rekcod

**Status:** built, 2026-10-13. `scripts/e2e-storage.sh` passes (25 checks). The in-instance
reach (below) needs a guest built after this change.

`platform.md` tracks 2 and 3 were specced around an S3 bucket and shipped without one. RustFS
is the bucket: a single static Rust binary, S3-compatible, Apache-2.0, run as a LaunchAgent
next to the controller like `controller serve`. No MinIO.

## Where it runs

`rekcod storage serve --listen 100.x.y.z:7710 [--data DIR] [--install]` downloads the pinned
release (`rustfs-macos-aarch64-v1.0.1.zip`, sha256 checked) to `~/.rekcod/bin/rustfs-1.0.1`,
strips the quarantine flag, and `exec`s it with root keys from
`~/.rekcod/storage/credentials.json` (written 0600 on first run, endpoint updated on every
start). Data defaults to `~/.rekcod/storage/data`. The unsigned binary runs fine from a
LaunchAgent; curl doesn't set the quarantine flag anyway.

`rekcod controller serve --storage http://...:7710` reads those keys (or
`--storage-credentials FILE`) and stores URL and keys in fleet.db's `config`, the secret
sealed with the `default` namespace key. Later starts without `--storage` use what's stored.
On start it creates `rekcod-releases`, `rekcod-cache` and `rekcod-backups`; a RustFS that
isn't up yet only logs, and deploys create the buckets they need.

`rekcod spare setup` runs `storage serve --install` on `ADDR:7710` before the controller and
passes `--storage`. One RustFS on one Mac: that disk is a single point of failure, which
`platform.md` says not to rely on. RustFS's distributed mode is the upgrade path; I haven't
measured it on Macs.

## The client

`crates/rekcod/src/s3.rs` is a small SigV4 client on `std` (checked against the AWS signing
examples): path-style requests, chunked responses, files streamed with `UNSIGNED-PAYLOAD`,
presigned GETs, and RustFS's admin API. RustFS 1.0.1 takes MinIO's admin calls under
`/rustfs/admin/v3/` with a plain JSON body (`add-user`, `add-canned-policy`,
`set-user-or-group-policy`, `remove-user`, `remove-canned-policy`). That was the open
question in the spec; it does.

## What uses it

1. **Per-app buckets.** `[storage] bucket = true` in `rekcod.toml` (sent as `bucket` on
   deploy) creates `app-<app key>` (a hash when that's over 63 characters), a user
   `app-<id>`, and a canned policy `bucket-<bucket>` that allows `s3:*` on that bucket only.
   Every instance gets `S3_ENDPOINT`, `S3_BUCKET`, `AWS_ACCESS_KEY_ID`,
   `AWS_SECRET_ACCESS_KEY`, `AWS_REGION=us-east-1`. The user and policy are written again on
   every deploy, so a RustFS that lost them gets them back. The secret is sealed in
   `apps.storage_secret` like app secrets. Previews of an app with a bucket get their own,
   deleted with the preview. `DELETE /apps/NAME` refuses a non-empty bucket unless
   `force`; the CLI asks on a terminal. `app storage NAME rotate` makes a new secret and
   redeploys the live commit.
2. **Releases without rebuilds.** The controller sends nodes a `store` with root keys and
   `rekcod-releases/<app>/<commit>`. The key is the commit, not the release number, because
   rollback and recycle make a new release of an old commit. After a build the node runs
   `docker save | zstd` to `<commit>.tar.zst`, then writes `<commit>.json` (the commit's
   rekcod.toml), whose presence means the pair is complete. If both exist the node loads
   the image and retags it instead of cloning and building. In a deploy with several
   instances and nothing stored, the first instance builds and pushes before the rest
   start, and they load. A failed push only logs. Node log lines say `cloning` or
   `loading`, which is what the e2e checks.
3. **BuildKit cache.** dockerd's builder doesn't have the `s3` cache exporter ("unknown
   cache exporter"), so this deviates from the spec: the node runs `docker buildx build
   --cache-from/--cache-to type=local` with directories on the Mac, and keeps them in
   `rekcod-cache/<app>.tar.zst`. As a bonus the sandbox never talks to RustFS for this. No
   buildx falls back to a plain build.
4. **Backups.** The watchdog uploads a `VACUUM INTO` copy of fleet.db to
   `rekcod-backups/fleet-<UTC time>.db` every hour and keeps 48; the attempt is recorded
   first, so a failure retries in an hour. `controller backup` and `controller backups` do
   and list them; `controller restore [KEY]` (controller stopped) downloads the latest,
   checks it opens, moves the old fleet.db aside as `fleet.db.before-restore-<time>`, and
   puts it in place. `controller/secret-key` isn't in the backup.
5. **Snapshots.** `snap save --push` uploads each snapshot volume as
   `snaps/<project>/<name>/<volume>.tar.zst` plus a `manifest.json` with the volumes' labels;
   `snap restore --pull` recreates the snapshot from them, then restores as usual. The
   target is `$S3_ENDPOINT`/`$S3_BUCKET` with the `AWS_*` keys, else the local RustFS and
   bucket `rekcod-snaps`. This is the CLI side; a pinned app moving between nodes isn't wired
   to it.
6. **Local dev.** `rekcod storage up` runs `rustfs/rustfs:1.0.1` in the VM as
   `rekcod-storage` on `127.0.0.1:<port>`, keeping its old port when that's still free and
   otherwise taking the first free one from 9000. Data is in volume `rekcod-storage`, keys in
   `~/.rekcod/storage/local.json`. It prints the env block; `storage env` prints it again,
   `storage down` removes the container. The menu bar has Start / show env / Stop
   (`storage_actions`).

## Reaching RustFS from an instance

Sandbox guests reject everything on the Mac, the LAN and 100.64.0.0/10 (`boot.sh`), which
includes RustFS on the tailnet. A sandbox now has `reach: ["IP:PORT"]` in its meta, passed to
the daemon as `--reach` and to the guest as `rekcod.reach=IP:PORT,...` on the kernel command
line (checked to be `IPv4:PORT` before it goes there). `boot.sh` inserts an ACCEPT for each,
TCP to that port only, at the top of `OUTPUT` and `DOCKER-USER`. A node sets it to the S3
endpoint for instances of apps with a bucket; the sandbox API never does.

This needs a guest image with the new `boot.sh`. I couldn't build one while writing this (the
VM had no internet behind a full-tunnel VPN), so it's untested end to end. The e2e reads the
instance's env and uses its keys from the Mac instead.

## Tenancy

Each app's key can only touch its own bucket (the e2e checks a second app gets 403). Root keys
stay on the controller and go to nodes in the deploy spec, over the same node-secret channel
as app secrets; they're never in an instance's env. A node is trusted with them anyway, since
it runs the controller's builds.

## CLI

```sh
rekcod storage serve [--listen IP:PORT] [--data DIR] [--install] | uninstall
rekcod storage up | down | env
rekcod controller serve ... --storage URL [--storage-credentials FILE]
rekcod controller backup | backups | restore [KEY] [--db PATH]
rekcod app storage NAME [ls | url KEY [--expires 1h] | rotate]
rekcod app destroy NAME [--force]
rekcod snap save|restore ... [--push|--pull]
```

## Limitations

- Releases, cache and buckets don't expire. Old commits' images stay in `rekcod-releases`
  until someone deletes them.
- Big images go through a temp file on the node's disk on the way up and down.
- `storage serve` binds one address. Instances use that address, so it has to be one their
  node can route to (the Tailscale IP on a spare); `127.0.0.1` only works for the Mac's own
  processes.

## Verification

`scripts/e2e-storage.sh`: storage serve; deploy with `bucket = true` and two instances (one
build, one load); the instance's env; its key writes and reads; a second app's key gets 403;
presigned URL; build cache object; rollback loads with no `git clone` in the node log;
rotate makes the old secret fail; destroy refuses a non-empty bucket, `--force` deletes it;
backup, restore, restart. `snap --push/--pull` and `storage up` were checked by hand against
the main VM.
