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
  deleted, `draining` ones are deleted after the drain time, jobs whose node doesn't know
  them become `lost`.
- The first lost-node check waits one heartbeat interval after startup.
- Nodes boot their instance VMs again when they start (`node serve` walks its sandboxes
  that hold an `instance.json` and starts them plus their relays).
- Jobs a node lost (no `result.json`, no running thread) are reported failed on the next
  `job_get`; the controller stops counting them against quota.
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
- `controller restore` marks every instance `lost` so the first reconcile checks them.

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

Two CAs: `ca.pem` signs only the controller's certificate and is what CLIs and edges pin;
`node-ca.pem` signs node certificates and only the controller trusts it, so a node (which
gets its certificate at registration, before approval) can't pass for the controller. A
node can't register the controller's own names or an address another node holds, and at
most 20 nodes wait for approval at once. App instance sandboxes are reachable only through
their app, never through `/sandboxes/ID`. Edges report traffic and wake apps for every
app, by design: an edge token is trusted for that.

## Status

Built as described, except: WebSocket tunnels close after an hour idle but the edge's
trusted-proxy default is still loopback whatever `--public-listen` is. `scripts/e2e-fleet.sh`
runs the fleet over https with node approval and an edge token.
