# Fleet HA

**Status:** placement, replica recovery and two-edge provisioning verified, 2026-10-17.
Controller standby, storage replication and lost-job retries remain plans. Configure an
external ingress or clients to fail over between the edges.

What it takes for a fleet of Macs to keep serving apps, and keep accepting deploys, when any
one Mac dies. Builds on [`fleet-hardening.md`](fleet-hardening.md) (reconcile, lost nodes)
and [`rustfs.md`](rustfs.md).

## Where it stands

Read from `crates/rekcod/src/fleet.rs` and `edge/`, not from the older specs.

| Piece | Copies | When its Mac dies |
|---|---|---|
| App instances | `instances` per app, on nodes from `place()` | Edge skips them on the next connect (3 s timeout, out for 5 s). Node marked lost after 15 s; reconcile tops up the live release after 60 s short |
| Node | as many Macs as you have | Its sandboxes, instances and jobs go `lost`. Only instances come back elsewhere |
| Edge | one per `edge serve`; multiple supported | Ingress or clients must select another surviving edge |
| Controller | one, SQLite in `controller/fleet.db` | Apps keep serving from each edge's `edge-routes.json`. No deploys, wakes, reconcile, jobs, schedules or webhooks until it's back |
| RustFS | one, usually on the controller's Mac | Release images, build cache, backups and app buckets are gone with that disk. Deploys fall back to building from git; apps using their bucket break |

Placement now reserves memory per assignment and spreads across zones and nodes. Recovery
starts only missing instances, keeps partial progress, and skips failed nodes for the current
attempt. Healthy replicas keep their sandbox IDs and release number.

Remaining controller limitations:

1. **Nodes, edges and CLIs know one controller URL** (`NodeConfig.controller`,
   `Remote.url`). There's nowhere to put a second one.
2. **The standby would need the controller's private material**: `secret-key` (seals
   secrets), `ca.pem`/key and `node-ca.pem`/key (what nodes and CLIs pin), `admin-token`.
   None of it is in fleet.db.
3. **Backups are hourly** (`backup_due`), to RustFS and/or `--backup-dir`. Up to an hour of
   deploys, tokens and secrets is lost on restore, and the restore is by hand.

Not in scope: stateful sandboxes and volumes (their disk is on one Mac, as `platform.md`
says), databases (keep them off the fleet), more than one controller writing at once.

## Pieces, in build order

### 1. Placement: spread and top up (implemented)

- `node join --zone rack-a` stores the node's failure domain in `nodes.zone`. Without a
  label, a Mac counts as its own domain. Placement ranks candidates by app replicas in the
  zone, then on the node, then free memory, and reserves memory for each assignment.
- `[resources] spread = "soft"` is the default. Shared Macs and labelled zones appear in
  `app status`'s `placement` field and the release log. `spread = "strict"` requires a
  separate Mac and failure domain for every replica of the new release; failure leaves
  the previous release serving. An old release does not block rolling placement.
- Reconcile tops up the live release, without creating a release or restarting survivors.
  A failed launch or health check excludes that node for the current attempt and tries
  another. Successful partial recovery stays live. Remaining replicas retry after 60 s;
  capacity alerts are sent once until recovery completes.
- Replaced lost replicas are retired, including their sandbox records, so a returning node
  cannot bring them back. Replicas not yet replaced can return after a successful check.
- `node drain` tops up before removing routes to old replicas. With insufficient capacity,
  existing replicas keep serving and reconcile retries the move. `remaining_instances`
  reports outstanding moves. Explicit drain can wake sleeping apps; ordinary reconcile
  leaves sleeping apps asleep. Other sandboxes and their disks stay on the draining Mac.

Controller tests exercise memory accounting, zones, strict placement, pools, partial
recovery, launch retry, safe drain and returning nodes through SQLite and loopback HTTP.
`scripts/e2e-fleet.sh` adds a three-node deploy and worker loss: all three replicas recover
on the surviving two Macs, with the release number and both surviving sandbox IDs unchanged.
Soft spreading permits sharing when the failure leaves fewer Macs than replicas.

### 2. Two edges (implemented)

`server setup --edge-only --remote NAME --join-token TOKEN` installs an additional node
and edge against an existing admin remote. It creates a restricted edge token tied to the
node ID and installs no local controller, storage service or main Docker VM. Repeating
setup preserves the node registration; a different controller or address causes an error
before service changes. The join token is only needed for a new node.

The controller already sums traffic and reported clients from multiple edges, unions
active hosts and retains the worst reported percentiles. HTTP tests verify this behavior.
Wakes from different edges still share the controller's app lock. Rate limits, in-flight
connections and unfinished-minute statistics stay on each edge.

`scripts/e2e-edges.py` runs two edge processes and a controller, checks provisioning and
scoped tokens without loading launchd jobs, verifies that metrics and clients sum 12 + 8
requests, then terminates one edge during 100 requests through a client with failover.
The survivor serves every request. CI runs this check without guest VMs.

[Two edges](fleet-edges.md) documents connector replicas of the same Cloudflare tunnel
and tailnet clients with retries or a health-aware load balancer. The process test does
not configure or test Cloudflare. A host outage drops its connector; an edge-only process
failure needs origin health checks or supervision of the connector with the edge.

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

1 and 2 are implemented. 3 is most of the work and
only pays off with 3+ nodes. 4 needs research before it's a plan. 5 is small and can go
anywhere.

## Open questions

- Should the standby also run its own RustFS and edge, so "second Mac" means one command?
  I lean yes.
- Snapshots versus WAL shipping: decide after measuring snapshot size and time on a fleet.db
  with 90 days of audit rows.
- Can `bonjour` advertise both controllers so `lan-discovery.md` finds whichever is primary?
