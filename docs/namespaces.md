# Namespaces: multi-tenancy inside one fleet

**Status:** built 2026-10-13, checked by `scripts/e2e-fleet.sh` ("namespaces") and
`fleet::tests::namespaces_roles_quotas_pools_and_secrets`. Builds on tracks 2 and 3 in
[`platform.md`](platform.md).

Namespaces split one fleet between teams or projects of the same company sharing Macs they
own. Selling the fleet to outside tenants is still out (see "Hosted multi-tenant: not
planned" in `platform.md`).

## Model

| Thing | Scope |
|---|---|
| Node | fleet; optionally pooled to namespaces |
| Token | fleet: one per person or bot |
| Namespace | name, quota, secret salt |
| Membership | (namespace, token name, role) in `members` |
| App, release, instance, sandbox, job, schedule, secret, env | namespace |
| Audit row | has an `ns` column; `-` for fleet-wide actions |

Roles:
- `viewer`: `app ls/status/logs`, `sandbox ls/diff`, `job ls/logs` (every GET).
- `member`: also deploy, env, secrets, sandboxes, jobs.
- `owner`: also adds and removes members.

Fleet admin tokens own every namespace, and only they can create or delete namespaces,
change quotas and pool nodes.

`default` is special. Every token is a member of it, and there each person only sees their
own sandboxes, apps and jobs (`can_touch`), which is how the fleet worked before
namespaces. In any other namespace members share everything. So upgrading changes nothing
for existing users.

Names (namespaces, apps, schedules) are DNS labels of at most 40 characters without `--`.

## Hostnames and keys

`<app>--<ns>.<domain>`, for example `api--billing.apps.example.com`. `default` keeps
`<app>.<domain>`.

One label keeps a single `*.<domain>` wildcard certificate (DNS-01) valid for everything.
The same string is the app's primary key in `apps.name` (`app_key`), so releases,
instances, the edge's activity reports and the per-app locks didn't change, and no table
had to be rebuilt. Since neither part can contain `--`, the key is unambiguous.
`<app>--<ns>` must fit in 63 characters. PR previews are `<app>-pr-<n>--<ns>`, created in
the base app's namespace. Schedules use the same key scheme for their primary key.

## Data

Added in `MIGRATIONS` (an `ns` column defaulting to `'default'` on `apps`, `sandboxes`,
`jobs`, `schedules`, `audit`; `cpus` and `memory_gib` on `sandboxes`) and in `SCHEMA`:

```sql
create table namespaces (name text primary key, created integer not null, salt blob not null,
  max_sandboxes integer not null default 10, max_cpus integer not null default 16,
  max_memory_gib integer not null default 32, max_apps integer not null default 20);
create table members (ns text not null, who text not null, role text not null, primary key (ns, who));
create table node_pools (node text not null, ns text not null, primary key (node, ns));
```

`default` is inserted on open.

## API

Scoped routes take a `/ns/{ns}` prefix: `/ns/team/apps/web/deploy`. Without the prefix
they're `default`, so older CLIs keep working. The CLI always sends the prefix.

Fleet-wide: `GET/POST /namespaces`, `GET/DELETE /namespaces/{n}`,
`POST /namespaces/{n}/quota`, `POST /namespaces/{n}/members`,
`DELETE /namespaces/{n}/members/{who}`, `POST /nodes/{id}/pool` (`{"ns": [...]}`),
`POST /tokens` with optional `ns` and `role`.

`Controller::allow(who, ns, role)` is the one check. A namespace the caller isn't in
answers `no namespace X` (404), the same as one that doesn't exist. Inside a namespace a
missing app, sandbox or job is `no app X` and so on.

## Quotas

`check_quota` runs before a sandbox, job or fork starts, and in `upsert_app` before a
deploy or scale. Usage counts:
- apps with a live release, at `instances * cpus` and `instances * memory_gib`. Sleeping
  apps count, so a wake never fails on quota. A redeploy replaces the app's own share.
- running sandboxes (not app instances), at their `cpus`/`memory_gib`
- running jobs, at their spec's `cpus`/`memory_gib`

`max_sandboxes` covers sandboxes plus jobs. `max_apps` counts previews. The per-token
`max_sandboxes` still applies on top.

Any limit, the per-token one included, can be `unlimited` on the CLI
(`ns create/quota --max-cpus unlimited`, `controller token NAME --max-sandboxes unlimited`;
`controller limit NAME --max-sandboxes N` changes an existing token's cap).
It's stored as -1, the checks skip negative limits, and `ns ls` shows `"unlimited"`. Node
capacity still applies: placement needs a node with the memory free.

## Isolation

- **Compute.** Every instance and sandbox was already its own VM.
- **Network.** A node's instance relays used to accept anyone who could reach
  `node-ip:port`, so a tailnet peer could skip the edge and the private/public split. Now
  the relay accepts only:
  - loopback and the node's own address
  - the controller's addresses (resolved from its URL)
  - edges, which are addresses that called `GET /routes` in the last 10 minutes

  The controller hands the edge list to each node in the heartbeat answer (every 5 s).
  Guests were already kept off the LAN by their egress rules.
- **Secrets.** AES-256-GCM, values stored as `sealed1:<hex nonce+ciphertext>`. The key is
  HMAC-SHA256 of `controller/secret-key` (32 bytes, a file next to `fleet.db`, not inside
  it) and the namespace's salt. Deleting a namespace deletes the salt, so a backup row from
  it can't be opened. Plain secrets from before this are sealed when the controller starts.
- **Node pools.** A namespace with a pool is placed only on its pool, and other namespaces
  never land on pooled nodes.

## CLI

The namespace comes from `--ns`, then `REKCOD_NS`, then the remote's `ns` (`rekcod remote
add ... --ns team`, or `rekcod ns use team`), then `default`.

```sh
rekcod ns create billing [--max-cpus 16 --max-memory 32 --max-sandboxes 10 --max-apps 20]
rekcod ns ls
rekcod ns member add billing alice [--role viewer|member|owner]
rekcod ns member rm billing alice
rekcod ns quota billing --max-cpus 24
rekcod ns use billing
rekcod ns rm billing [--force]
rekcod node pool NODE --ns billing [--ns other]    # no --ns: unpool
rekcod controller token dave --ns billing --role viewer
rekcod app|sandbox|job ... --ns billing
```

The menu bar lists the current namespace's apps and jobs. When the token is in more than one
namespace it shows "Namespace: X" with a "Switch to Y" item for each other one
(`rekcod ns use Y`, `Kind::NsUse`).

## Not done

- **Edge login.** Private apps are reachable by anyone who can reach the edge's `--listen`,
  so keep it on the tailnet. The edge has no session cookie yet that could be checked
  against membership.
- **Owners changing their own quota.** Quotas are set by admins only.
- **Outside customers and billing** (licence, see `platform.md`).
- **Nested namespaces and per-app ACLs.**
- **SSO/OIDC.**
- **Per-namespace domains.** There's one `--domain` per controller.
