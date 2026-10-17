# Fleet HA

**Status:** plan, 2026-10-16. Nothing here is built yet.

What it takes for a fleet of Macs to keep serving apps, and keep accepting deploys, when any
one Mac dies. Builds on [`fleet-hardening.md`](fleet-hardening.md) (reconcile, lost nodes)
and [`rustfs.md`](rustfs.md).

## Where it stands

Read from `crates/rekcod/src/fleet.rs` and `edge/`, not from the older specs.

| Piece | Copies | When its Mac dies |
|---|---|---|
| App instances | `instances` per app, on nodes from `place()` | Edge skips them on the next connect (3 s timeout, out for 5 s). Node marked lost after 15 s; reconcile replaces after 60 s more |
| Node | as many Macs as you have | Its sandboxes, instances and jobs go `lost`. Only instances come back elsewhere |
| Edge | one per `edge serve` | Public traffic stops unless something in front fails over |
| Controller | one, SQLite in `controller/fleet.db` | Apps keep serving from each edge's `edge-routes.json`. No deploys, wakes, reconcile, jobs, schedules or webhooks until it's back |
| RustFS | one, usually on the controller's Mac | Release images, build cache, backups and app buckets are gone with that disk. Deploys fall back to building from git; apps using their bucket break |

Problems I found in the code, beyond "one controller":

1. **Spreading is round-robin over a list, not anti-affinity.** `deploy_locked` puts instance
   `i` on `nodes[i % nodes.len()]` (`fleet.rs:2901`). `place()` filters on room for *one*
   instance, so a node with room for one can get two. Two instances can land on one Mac
   while another sits idle.
2. **Reconcile redeploys everything to replace one instance.** `reconcile` calls
   `deploy_locked` (`fleet.rs:1882`), which makes a new release, starts `instances` new VMs
   and drains all the healthy ones. Losing 1 of 5 instances starts 5 VMs.
3. **Deploys are all-or-nothing per node.** One node failing `POST /instances` deletes every
   instance the deploy started (`fleet.rs:2969`). A flaky node fails every deploy, and
   reconcile then retries every 60 s and alerts each time.
4. **No partial capacity.** If the surviving nodes fit 2 of 3 instances, reconcile fails and
   the app stays at whatever survived, rather than going back up to 2.
5. **Nodes, edges and CLIs know one controller URL** (`NodeConfig.controller`,
   `Remote.url`). There's nowhere to put a second one.
6. **The standby would need the controller's private material**: `secret-key` (seals
   secrets), `ca.pem`/key and `node-ca.pem`/key (what nodes and CLIs pin), `admin-token`.
   None of it is in fleet.db.
7. **Backups are hourly** (`backup_due`), to RustFS and/or `--backup-dir`. Up to an hour of
   deploys, tokens and secrets is lost on restore, and the restore is by hand.

Not in scope: stateful sandboxes and volumes (their disk is on one Mac, as `platform.md`
says), databases (keep them off the fleet), more than one controller writing at once.

## Pieces, in build order

### 1. Placement: spread and top up

The cheapest change with the most effect. All in `fleet.rs`.

- `assign(app, count, exclude)`: for each instance, pick the ready node with the fewest
  instances of this app, then the most free memory, then subtract `memory_gib` from that
  node's free memory before picking the next. Replaces the `nodes[i % len]` loop.
- Nodes get an optional `zone` (`node join --zone rack-a`, stored in `nodes.zone`).
  `assign` spreads across zones first, then nodes. A zone is whatever fails together: a
  power strip, a shelf, a building.
- Soft by default. When two instances share a node (or zone) the deploy log says so and
  `app status` shows it. `[resources] spread = "strict"` fails the deploy instead.
- Reconcile tops up: it starts only the missing instances of the *live* release on nodes
  `assign` picks, waits for health, marks them live. No new release number, no drain. Same
  code path as a deploy's launch step, factored out of `deploy_locked`.
- A node that fails `POST /instances` during a deploy or top-up is skipped and the instance
  retried on the next node `assign` gives. The deploy fails only when no node is left.
- Partial top-up: if only 1 of 2 missing instances fits, start that one and alert once.
  Retry the rest on the next pass, not every 60 s with an alert each time.
- `node drain` moves instances off with a top-up first, then drains, so a drain doesn't
  drop the app below `instances`.

Tests: `place`/`assign` unit tests next to the existing ones at `fleet.rs:4769`
(two-per-node never while a node is empty, zones before nodes, memory subtracted per
assignment). `e2e-fleet.sh`: 3 instances on 3 nodes land on 3 nodes; kill one node, the
replacement goes to a node without one and the release number doesn't change.

### 2. Two edges

Mostly setup and docs. The edge already keeps no state that matters: routes come from the
controller, bans too, and rate limits and strike counts being per edge is fine.

- Public: run `cloudflared` on two Macs as connectors of the same tunnel, each pointing at
  its local edge. Cloudflare fails over between connectors.
- Tailnet: `--listen` on each edge Mac, and a name with both addresses, or clients that pick.
- Check the controller sums `/routes/activity` traffic and clients from two edges instead of
  one overwriting the other. Activity hosts are a union already. Wakes from two edges
  share one wake through the controller's app lock.
- `server setup` gains `--edge-only` for a Mac that runs a node and an edge but no
  controller.

Tests: e2e with two edges; kill one mid-load, the other keeps returning 200s;
`/metrics` request counts equal the sum from both.

### 3. Controller standby

The big one. One primary writes; one standby holds a recent copy and takes over when the
primary is gone. No consensus library, no Postgres. SQLite stays.

**Who's primary: leases granted by the nodes.** Nodes already talk to the controller every
5 s, and there are usually three or more, so they're the witnesses.

- Each node keeps one grant: `{holder, term, until}`. `POST /lease {holder, term}` from a
  controller succeeds if the grant is free, expired, or already held by that controller at
  that term. A granted lease lasts 15 s.
- The primary renews with every node every 5 s. It's primary while a majority of approved
  nodes granted it within the last 10 s. That's 5 s less than the nodes hold the grant, so
  the old primary stops before any node will grant someone else.
- A primary that loses its majority stops writing: watchdog duties stop, API writes answer
  `421` with `{"primary": URL}` if it knows one.
- The standby asks for a lease at `term + 1` once the primary has been unreachable for 20 s.
  Nodes grant it once their old grant expires. With a majority it promotes.
- **Fencing.** Every controller-to-node call carries `X-Rekcod-Term`. A node remembers the
  highest term it has granted and refuses calls with a lower one. An old primary that
  comes back after a pause can't delete instances.
- Majority of approved nodes means 3 nodes ride out one Mac (controller plus its node)
  dying. With 2 nodes, failover needs both, so it's no better than one controller. `doctor`
  says so.

**Replication: snapshots, pushed on writes that matter.**

- The standby pulls `GET /replica/snapshot` (a `VACUUM INTO` copy, same as backups) every
  5 s when the primary's `data_version` changed. fleet.db is audit rows, release logs and
  config, so I expect it to stay in the low MBs. I'll measure on the e2e fleet before
  deciding on anything finer like shipping WAL frames.
- A deploy finishing, a token or secret changing, a node being approved: the primary pushes
  a snapshot to the standby before answering, waits up to 2 s for its ack, and on timeout
  answers anyway and alerts. Without this, a deploy that finished in the gap gets rolled back
  by the standby's reconcile. The standby still thinks the old release is live, finds its
  instances gone, and redeploys the old commit.
- On promote: bump `term`, run the startup sweep, mark every instance `lost` (what `restore`
  already does), and let the first reconcile take back the ones that answer. Sandboxes the
  standby doesn't know about are deleted after 10 minutes, as today. That's the cleanup for
  whatever the primary started in the last seconds.

**Setup.**

- `rekcod controller standby --from URL` (admin token) fetches the private bundle once over
  the pinned TLS connection: `secret-key`, both CA keys, `admin-token`, storage credentials.
  Written `0600` next to its fleet.db. Same CA on both, so nothing re-pins.
- The primary issues itself a certificate that also names the standby's host, and the
  standby does the same, so either answers on either name.
- `server setup --standby` / `host add --server --standby`: node, edge and a standby
  controller on a second Mac.

**Clients find the primary.**

- `node.json` `controller` becomes `controllers: [..]` (old field still read). `remote add`
  takes a comma-separated list. Edges use their remote's list.
- `call`/`send` try the last URL that worked, then the others; a `421` with `primary` goes
  there. A standby answers reads it can serve (`/routes`, `/ca.pem`) and `421` for the rest.
- Heartbeats go to every controller in the list, so the standby knows which nodes are up
  before it promotes.

**Frontends.** Per `CLAUDE.md`, any new action is a `Kind`: `controller standby`, `controller
promote` (by hand, for planned maintenance), `controller step-down`. They go in
`controller_actions`, `state.rs` shows primary/standby/term, and `docs/frontends.md` and the
README get updated in the same change.

Tests: e2e with a primary, a standby and three nodes:
- kill the primary mid-load: edges keep serving, the standby promotes within 35 s, a deploy
  then works through the same `remote`;
- deploy, kill the primary right after it answers: the new release is live after failover,
  not the old one;
- pause the old primary (`SIGSTOP`) past its lease, resume it: it answers 421, and nodes
  refuse its calls (term check).

### 4. Storage

RustFS on the controller's Mac is the other single disk. What's on it, and how bad losing it is:

- release images and build cache: deploys rebuild from git, so it's slow but not broken;
- `rekcod-backups`: still on `--backup-dir` if that's set somewhere else;
- app buckets: app data, actually lost.

Options I want to check before choosing:

- RustFS distributed mode across the Macs. The usage limits put write quorum at N/2+1, so it
  needs at least 3 servers to survive one going. Its erasure coding protects drives in a set,
  not necessarily whole nodes, so layout matters. Not measured on Macs.
- RustFS bucket replication from the primary's RustFS to one on the standby's Mac. The 1.0
  release lists site and bucket replication, but I haven't found what it needs or how it
  fails over.

Until then: point `--backup-dir` at a disk on another Mac, and use an external S3 for app
buckets that hold anything you'd miss.

### 5. Jobs on a lost node

`job run --retries N` (default 0): a job lost with its node is started again elsewhere, up to
N times. Schedules already start the next run regardless.

## Order and cut lines

1 and 2 are worth doing on their own and don't depend on anything. 3 is most of the work and
only pays off with 3+ nodes. 4 needs research before it's a plan. 5 is small and can go
anywhere.

## Open questions

- Should the standby also run its own RustFS and edge, so "second Mac" means one command?
  I lean yes.
- Snapshots versus WAL shipping: decide after measuring snapshot size and time on a fleet.db
  with 90 days of audit rows.
- Can `bonjour` advertise both controllers so `lan-discovery.md` finds whichever is primary?
