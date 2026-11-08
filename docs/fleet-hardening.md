# Fleet hardening

What changes in the controller, nodes and edge so the fleet can host public apps. Each item
comes from `docs/production-readiness.md` sections 4, 5 and 8. Section 1 (git injection,
`?id=`, create-body passthrough) is already fixed.

## Tokens and roles

- Tokens are stored as SHA-256 hashes (`tokens.hash`). The plaintext is shown once, when
  it's made. On first start after the upgrade the controller hashes the existing rows in
  place, so old tokens keep working.
- `rekcod controller token rm NAME` deletes every token of a person; `--expires 30d` on
  create sets `tokens.expires`. An expired token answers 401 like a wrong one.
- `tokens.default_role`: what the token may do in `default`. `controller token alice`
  keeps today's behaviour (`member`). A token made with `--ns team` gets `none`, so a
  viewer in `team` can't deploy into `default`.
- A new `edge` kind of token (`controller token edge-1 --edge`) may only read `/routes`
  and `/bans` and post `/routes/activity`. `server setup` makes one for its edge instead
  of handing it the admin token.
- Viewers no longer see env values in app status, only the keys.

## Nodes

- A node registers as `pending`. `rekcod node approve ID` (admin) makes it `ready`;
  `controller serve --auto-approve` keeps today's behaviour for one-Mac fleets and
  `server setup` passes it.
- `rekcod node rm ID` deletes a node (its instances are rescheduled).
- `rekcod controller rotate-join` makes a new join token; the old one stops working.
- Placement trusts `mem_free` only up to the `mem_total` the node sent at join.
- Nodes build endpoints from the host in `--advertise`, never from the listen address, so
  `--listen 0.0.0.0:7701 --advertise http://mac2:7701` works.
- A node reads the files of a cloned repo on the Mac (`rekcod.toml`, the version files
  runtime detection looks at) only as regular files directly in the checkout, without
  following a symlink and at most 1 MiB each. A `rekcod.toml` that's a link fails the
  deploy; a version file that's a link is skipped. The generated Dockerfile is created
  new (`O_EXCL`, `O_NOFOLLOW`), and a checkout with anything named `Dockerfile`, even a
  dangling link, gets none: `docker build` keeps a linked Dockerfile inside the context.

## Secrets on the wire

- Nodes no longer get the RustFS root keys. The deploy spec carries presigned GET/PUT URLs
  for the release image and build cache (`s3::presign` with a 1 h expiry).
- A missing `controller/secret-key` is only created when fleet.db holds no sealed values;
  otherwise the controller refuses to start and says where the key should be.
- `GITHUB_TOKEN` goes to curl on stdin (`-K -`), not argv.

## Webhooks

- `X-GitHub-Delivery` ids are kept for 24 h; a repeat is ignored.
- `closed` only destroys apps whose `preview_of` matches, never a real app with that name.
- Pull requests from forks don't deploy unless the PR has the `rekcod-preview` label.
- `opened`, `synchronize`, `reopened` and `labeled` act with admin rights, so `upsert_app`
  only takes `<app>-pr-<n>` when no app has that name or it's already `preview_of` that
  base app; otherwise the delivery fails and the other app, its secrets included, is left
  as it was. The check and the write are one transaction, under the preview's app lock
  through to the deploy.

## Apps

- Each app row has a `generation`, a counter value from `config` (`app_generation`)
  that's new when the row is made and again when its destroy begins. A deploy, top-up or
  wake compares the generation it loaded with the row after it takes the app lock, again
  in the transaction that registers each instance, and again in the one that makes the
  release live. On a mismatch it stops: an instance the node just started is deleted on
  the node without being recorded. A deploy that started for an app that was destroyed
  and made again by someone else can't put its VM into the new app's routes.
- Destroy sets `destroying` and a new generation first, which hides the app from routes,
  lists, quota, reconcile, idle stop and recycling, and keeps the name from being deployed.
  Then it takes the app lock, so it runs after a deploy in flight (which stops at its next
  check), deletes the instances, then storage (docs/rustfs.md), then the rows, kept logs
  and traffic. When storage cleanup fails the row stays `destroying`, the request fails
  with the reason, and the reconcile loop retries once a minute. The row is removed only
  for the generation that destroy marked, so a second destroy or a retry never removes an
  app someone made of that name afterwards. A controller started without `--storage`
  can't reach RustFS: it finishes the destroy and notes the app's key and bucket in
  `leftover_storage`, which a controller with storage removes on its next retry; that
  bucket name isn't handed out before then.
- Jobs marked `lost` by a controller from before this change (they got `finished`) are
  ended as `failed` by a migration, so they don't hold quota or a schedule's turn.
- Schedules are checked like one-off jobs when they're made (`cmd`, `git.url`, `cpus`,
  `memory_gib`), rather than failing at every run.
- Spec fields are checked before anything is stored: `instances` 1 to 100, `cpus` 1 to
  64, `memory_gib` 1 to 512, `port` 0 (rekcod.toml decides) or 1 to 65535,
  `idle_stop_secs` and `recycle_secs` 0 or more, `rollback`'s release a 32-bit number.
  Zero, negatives, fractions and strings are refused rather than narrowed. Quota sums
  saturate instead of wrapping. Remote sandbox creates get the same `cpus` and
  `memory_gib` ranges.
- Admission (docs/namespaces.md, "Quotas") covers creates, updates, scales and deploys of
  a ref; an admitted deploy holds its share until it ends.

## Convergence

The controller keeps desired state (apps' live release and instance count) and moves
actual state toward it. A `reconcile` pass runs every 10 s and on startup:

- Instances on a node that comes back from `lost` are health-checked and set back to
  `live` when they answer.
- An app with fewer healthy `live` instances than `instances`, for longer than 60 s, gets
  replacements on other nodes (same release, loaded from the release store).
- Heartbeats list the node's sandboxes. Instances the controller thinks are live but the
  node doesn't have are marked `lost`; sandboxes the node has that the controller doesn't
  know about (older than 10 minutes) are deleted.
- Startup sweep: releases stuck in `building` become `failed`, `starting` instances are
  deleted, `draining` ones are deleted after the drain time.
- The first lost-node check waits one heartbeat interval after startup.
- Nodes boot their instance VMs again when they start (`node serve` walks its sandboxes
  that hold an `instance.json` and starts them plus their relays).
- Jobs are stored as `pending` before a node hears of them, and starts are idempotent by
  job ID, so a lost reply is sent again instead of leaving a job nobody tracks. A job whose
  node goes quiet is `lost`, not over: it keeps its quota and its schedule's turn until the
  node reports it finished or `node rm` removes the node. Cancel stays `cancelling` until
  the node confirms. Details in [`fleet-compute.md`](fleet-compute.md#jobs).
- `node serve` removes job sandboxes (marked `fleet-job`) that no job thread of its own
  runs, at startup and hourly. Their jobs (no `result.json`, no running thread) are
  reported failed on the next `job_get`.
- A node that answers it doesn't have a job ends it: `failed`, or `cancelled` if a cancel
  was waiting.
- Deploys hold the app lock for their whole run. Release numbers are allocated inside one
  transaction.

## Timeouts and limits

- Every outbound call (`send`, `call`, `proxy`, relays) has a read timeout: 30 s for API
  calls, 10 min for deploys and job output, none for proxied exec streams (they end when
  the command does).
- The watchdog runs each duty on its own thread, so one hung node can't stop lost-node
  detection or backups.
- The edge keeps its last good route table in `edge-routes.json` and starts from it, and a
  route response that doesn't parse leaves the table as it was.
- Nodes keep their relay allowlist in `node-allow.json` across restarts.
- Wakes are single-flight per app on the edge; after a failed wake the edge answers 503 for
  30 s instead of asking again.
- WebSocket tunnels count against the edge's connection cap and close after 1 h idle.
- Rate limits key IPv6 clients by /64. `--trusted-proxy` defaults to loopback only when the
  edge's public listener is on loopback; X-Forwarded-For is read right to left, skipping
  trusted proxies.
- Quota checks and inserts happen under one DB lock.

## Database

- `PRAGMA user_version` numbers migrations; a migration that fails stops startup.
- Backups run `VACUUM INTO` on a separate connection, outside the main lock.
- `controller restore` checks the download before it replaces anything: it opens it
  read-only, without creating or migrating a schema, and refuses an empty file, one that
  fails `PRAGMA integrity_check`, a schema newer than this rekcod, or one without the fleet
  tables and a join token (a database no controller ran on). Then it marks every instance
  `lost` so the first reconcile checks them.

## Operations

- `GET /metrics` (admin or edge token): Prometheus text with nodes by state, instances by
  state, deploys by result, jobs by state, request counts per app from the edges.
- `controller serve --alert-webhook URL` posts a JSON line when a node is lost, a deploy
  fails or a backup fails.
- SIGTERM: the controller stops taking requests, waits up to 30 s for deploys in flight,
  then exits. Nodes and edges stop accepting and finish in-flight requests.
- Cleanup: node `remote-ws/*` and `jobs/*` older than 7 days without a live sandbox go;
  RustFS keeps the last 10 releases per app and drops a destroyed app's releases and
  cache; `audit` and finished `jobs` rows older than 90 days go.

## Transport

The controller makes a CA on first start, nodes and remotes pin it by fingerprint, and every
control-plane hop (CLI, edge and nodes to the controller, the controller to nodes) is TLS
(`tls.rs`). Plain `http://` still works on loopback and tailnet addresses (100.64.0.0/10,
fd7a:115c:a1e0::/48, `*.ts.net`), and elsewhere only with `REKCOD_INSECURE=1`. App traffic
between the edge and instance relays stays plain TCP; nodes belong on a tailnet.
Issued server certificates include the authority key identifier required by strict X.509
clients, including newer Python/OpenSSL versions. Existing CA keys and fingerprints are reused.

Two CAs: `ca.pem` signs only the controller's certificate and is what CLIs and edges pin;
`node-ca.pem` signs node certificates and only the controller trusts it, so a node (which
gets its certificate at registration, before approval) can't pass for the controller. A
node's certificate names the node, `<node id>.node.rekcod.invalid`, not the host it
advertised, and the controller expects that name from the node it dials. Several nodes
can share a host (`scripts/e2e-fleet.sh` runs three on 127.0.0.1), and a join-token holder
who registers another port on a node's host gets a certificate that only passes for its
own node ID. Nodes that joined before this hold a certificate for their host; the
controller accepts that only from the nodes it had the first time it started with node
names (the `host_named_nodes` row in `config`), until they join again. A node can't
register the controller's own names or an address another node holds, and at most 20
nodes wait for approval at once. Registration checks the URL (`scheme://host:port`),
zone and memory and issues the certificate before it stores the node, so a refused one
leaves no row. App instance sandboxes are reachable only through
their app, never through `/sandboxes/ID`. Edges report traffic and wake apps for every
app, by design: an edge token is trusted for that.

## Status

Built as described, except: WebSocket tunnels close after an hour idle but the edge's
trusted-proxy default is still loopback whatever `--public-listen` is. `scripts/e2e-fleet.sh`
runs the fleet over https with node approval and an edge token.
