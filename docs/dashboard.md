# Dashboard

A web front end (`dashboard/`, Next.js + shadcn/ui) for everything the menu bar shows, plus
what the menu bar has no room for: per-app traffic, releases and deploy logs, instances,
nodes, schedules, images, volumes, the disk breakdown. It runs on your Mac and binds
127.0.0.1 only, because it runs `rekcod` commands as you.

## Running it

```sh
rekcod dashboard            # starts the bundled build (or finds the running one), opens it
rekcod dashboard --install  # the same at login (LaunchAgent dev.rekcod.dashboard)
```

`rekcod dashboard` runs `start.cjs` from the first of `$REKCOD_DASHBOARD_DIR`,
`~/.rekcod/dashboard`, `<bin>/../share/rekcod/dashboard` and this checkout's
`dashboard/.next/standalone` that has one, on the first free port from 4500. A build without
`start.cjs` predates the token, so it's skipped; rebuild with `npm run bundle -- --install`.
It needs Node (`node` on `PATH`, Homebrew's, or `$REKCOD_NODE`). A second call prints and
opens the running one's sign-in URL. Logs go to `~/.rekcod/logs/dashboard.log`.

From a checkout:

```sh
cd dashboard && npm ci
npm run dev                  # prints http://localhost:4500/auth?token=..., or the next free port
npm run bundle -- --install  # standalone build, copied to ~/.rekcod/dashboard
```

## Signing in

Every container can reach the Mac's 127.0.0.1 through `host.docker.internal`, and any
process can set `Host: localhost`, so listening on loopback keeps nothing out. Each launch
gets a token instead:

- `rekcod dashboard` makes 32 random bytes (hex), passes them to the server as
  `REKCOD_DASHBOARD_TOKEN` and writes `{"pid", "port", "token"}` to
  `~/.rekcod/run/dashboard.json`, created 0600. It opens
  `http://localhost:PORT/auth?token=TOKEN`, and so does a second call, from that file.
- `npm run dev` and `npm start` make their own token (or take `REKCOD_DASHBOARD_TOKEN`) and
  print the link. They don't write the file.
- `/auth` compares the token in constant time, sets `rekcod-dashboard-PORT` (HttpOnly,
  SameSite=Strict, Path=/, until the browser quits) and redirects to `/`. The port is in the
  name because cookies ignore ports, and two dashboards would overwrite each other's.
- `src/proxy.ts` checks the cookie on every request except build assets and icons. Without it
  a page answers 401 with "Open this with `rekcod dashboard`", an API route answers 401.
  A server started without a token (or one under 32 characters) answers 401 to everything.
- Restarting the server (or the LaunchAgent at login) makes a new token, so open pages get
  401 until you run `rekcod dashboard` again. Under the LaunchAgent the token stays out of
  `dashboard.log`.

`start.cjs` sets the listen address to 127.0.0.1 whatever `HOSTNAME` says and drops
connections from any non-loopback peer, then loads Next's `server.js`. Pages can't be framed
(`X-Frame-Options: DENY`, `frame-ancestors 'none'`).

The daemon still splices `host.docker.internal:PORT` to the dashboard's port; the token is
what stops a container there.

Release tarballs don't include it unless built with `scripts/package.sh --with-dashboard`,
which puts the standalone build in `share/rekcod/dashboard` (Node is still needed to run it).

- `REKCOD_BIN` picks the binary. Without it: `rekcod` on `PATH`, then `../target/release/rekcod`,
  then `../target/debug/rekcod`. `rekcod dashboard` passes its own path.
- `REKCOD_STATE_FILE=fixtures/state.json` serves a saved document instead of running rekcod.
  Every action becomes a dry run, the read-only routes return made-up data (logs, inspect,
  release logs, job runs, a screenshot) and the charts get an hour of generated stats. The
  fixture has a remote with apps, traffic, a failed release, a lost instance, a stale and a
  drained node, jobs, bans, routes and backups.
- A second dev copy next to a running one needs its own build dir: `NEXT_DIST_DIR=.next-fixture`.
- `scripts/dev-fleet.sh` (repo root) starts a throwaway controller, two nodes and an edge with
  a deployed app, a failed release, jobs and traffic, and prints how to point the dashboard at it.

## Data

`rekcod state` prints one JSON document: `actions::Model` (VM, containers, ports, compose
projects and snapshots, images, volumes, disk usage, agent sandboxes, docker hosts, and from
the default remote: apps, jobs, namespaces, nodes, bans) plus the fleet extras (`state.rs`),
with every item's actions attached as the `*_actions` lists produce them:

```json
{"kind": "ContainerStop", "label": "Stop", "argv": ["stop", "web"], "interactive": false, "confirm": null}
```

`open` actions carry `"url"` instead of `"argv"`. `rekcod state --local` leaves out every
controller call.

The server keeps the latest state and pushes it to every open page over `GET /api/events`
(server-sent events):

- It follows `rekcod events`. A container, image, volume or network event runs
  `rekcod state --local` 150 ms later and pushes it, with the fleet keys from the last full
  state. A container stopped in a terminal shows in the page in about half a second.
- A full `rekcod state` (fleet included) runs every 15 s.
- One `rekcod stats` process feeds the CPU and memory columns and charts. The server keeps an
  hour of samples at 5 s, so a reload keeps the history (`GET /api/stats?window=300|900|3600`).
  Each container keeps its colour for the server's lifetime.
- `rekcod events` and `rekcod stats` only run while the VM does (they're docker commands, which
  would boot it), and both stop a minute after the last page closes.
- When the event stream drops, the page polls `GET /api/state` every 3 s.

A controller that doesn't answer isn't an empty fleet: each failed call is in
`remote.errors` (`apps`, `jobs`, `nodes`, ...) and the first one in `remote.error`. The
banner on every page, the sidebar badges and "Needs attention" show it.

`/system/df` takes seconds (it sizes every volume), so its result is cached in
`~/.rekcod/run/df.json` for two minutes; only a full `rekcod state` measures again. Sizes on
the Images, Volumes and Disk pages say how old they are.

## Actions

The dashboard is a third front end under the rules in `docs/frontends.md`: it renders the
action lists and never changes anything except by running `rekcod <argv>`.

- `POST /api/run {argv, subject}` reloads `rekcod state` and refuses argv that isn't one of the
  actions currently offered. So the page can't run arbitrary commands, only buttons it shows.
  A new button means a new `Kind`, not a new route.
- Actions with `confirm` ask in a dialog first (removes, prunes with the bytes they free,
  rollbacks, drains, banning a client IP).
- Containers, images and volumes have a checkbox column (`x` ticks the focused row). The bar
  at the bottom offers the kinds every ticked row has, minus interactive and streamed ones. It
  runs each row's own action, one after another through `/api/run`; if any of them confirms,
  one dialog lists every command first. Cancelling keeps the selection.
- While an action runs its row dims, shows a spinner and disables its buttons. Each toast has
  View (or Details), which opens that run in Activity (`/activity?run=ID`).
- Log-like actions (`logs -f`, `app logs`, `job logs`, `sandbox diff`, `storage env`) stream on
  their own page, `/stream`, over `GET /api/stream` (same allow-list check). Shells,
  `socket install` (sudo), `host forward` and typing an address to ban open Terminal.app, as
  the menu bar does.
- Every run is kept on the server until it restarts (`/activity`): argv, exit code, duration,
  output. A failed one stays red in the top bar until you open it. Activity filters by failed
  runs, subject and text (`?failed=1&subject=web&q=...`). "Run again" shows only while the same
  argv is still offered, and goes through the same confirm.
- Cmd+K searches every page, every row and every offered action ("restart web", "logs dash",
  "disk"); rows open their detail page. What you picked recently comes first (an empty query
  lists it, then the pages), and an action that confirms shows its warning under it. Running
  one goes through the same confirm and allow-list path as the buttons.
- Each API route (`lib/guard.ts`) checks again, in order: `Host` is localhost (DNS
  rebinding, 403), the sign-in cookie (401), `Sec-Fetch-Site` is `same-origin` or `none`
  (403; a page on another localhost port is "same-site" and gets the SameSite=Strict
  cookie), `Origin`, when sent, is this host (403; `null` included), and POSTs carry
  `x-rekcod: 1` (403).
- At most 16 log streams (`/api/stream`, `/api/logs`, `/api/instance-logs`) run at once, each
  its own `rekcod` process; the 17th gets 429 until one closes.

Read-only views that need more than `rekcod state` use fixed routes. Each runs one known
command and only for a name or id in the current state; none takes argv from the page:

| Route | Runs |
|---|---|
| `/api/inspect?name=` | `rekcod inspect NAME` (container page; env values are masked until clicked) |
| `/api/logs?name=&since=&timestamps=1&follow=1` | `rekcod logs --tail 1000 [--follow] [--timestamps] [--since S] NAME`, `S` one of 1m, 10m, 1h, 6h, 24h |
| `/api/probe` | a 1 s HTTP HEAD to each forwarded TCP port (cached 10 s) |
| `/api/release?app=&n=` | `rekcod app release APP N` (deploy log) |
| `/api/instance-logs?app=&id=` | `rekcod app logs APP --instance ID` |
| `/api/job-history?name=` | `rekcod job ls --name N` (last 50 runs) |
| `/api/sandbox-stat?id=` | `rekcod sandbox diff --stat ID` |
| `/api/screenshot?id=` | `rekcod sandbox screenshot ID <tmp>` (running macOS sandboxes) |
| `/api/vm-history` | none; the `fs_requests` counters from each state the server read |
| `/api/node-history?id=` | none; a node's load, memory and disk from each full state the server read, the last hour |

## Pages

The layout is the dux console's: a sidebar, a 56 px top bar with where you are (`Apps › dash`),
live/refresh, Activity and the bell, and one centred column. The sidebar has the rekcod mark
with the VM's state (its menu has the VM and storage actions), Find (Cmd+K), Overview, the list
pages grouped as This Mac, Fleet and System with a count each (a red badge when something in
that list needs attention), and at the bottom the remote/namespace menu, the docker host menu
the theme switch (System, Light, Dark) and the table density (comfortable or compact), both
kept in `localStorage`; density is set on `<html data-density>` before first paint. Containers
and apps you pin (the Pin button on their page) sit under Overview with their state dot. Below 768 px the sidebar is a
drawer behind the menu button in the top bar. From 768 px up, Cmd+B (or the panel button left of
the breadcrumb) folds it into a 60 px rail of icons; a dot on an icon means that list needs
attention. The fold is kept in `localStorage` and applied before first paint. The browser tab reads like the breadcrumb
(`dash · Apps · rekcod`), with the number of broken things first when there are any:
`(2) Apps · rekcod`. The icon is `app/icon.svg`, the rekcod mark (it follows the system theme);
`app/apple-icon.png` is the same mark on a dark square for the home screen.

Cmd+K lists rows to go to, then actions to run (ones that confirm are red). Detail pages show
their first actions as buttons under the title and the rest in the `...` menu, also on a phone.

`/` has the tiles (CPU and Memory with a line of the chart window's total), "Needs attention",
the last five runs from Activity and the CPU and memory charts. An unknown path gets a 404 page inside the shell. Each kind of thing has a
list page with a filter (`?q=`): `/containers`, `/images`, `/volumes`, `/sandboxes`,
`/projects`, `/ports`, `/apps`, `/jobs`, `/fleet`. Headers with arrows sort: containers (name,
CPU, memory, status), images (name, size, created), volumes (name, size), apps (name, req/min,
5xx), jobs (name, started, took, result). A click goes ascending, descending, off; the sort is
in the URL (`?sort=mem`, `?sort=-size`). A row flashes for 1.5 s when its state changes (a
static tint with reduced motion). An empty list says what to run to fill it, with copy buttons. Old `/?tab=X&q=Y` links redirect to `/X?q=Y`.
Everything you open is its own page, not a drawer:

| Page | Shows |
|---|---|
| `/containers/NAME` | tabs (`?tab=`): Overview (inspect data, health and its last output, restarts, its own CPU/memory chart, mounts, networks, labels), Logs, Inspect (the raw JSON, env values masked), Env |
| `/apps` | adds a requests-per-minute line for the last hour and the hour's 5xx share (red from 1%) |
| `/jobs` | a 24 h timeline above the table: one lane per job name, a bar per run coloured by result |
| `/fleet` | tabs (`?view=`): Nodes, Namespaces, Edge routes, Backups, Bans, Remotes and hosts; a tab only shows when there's something in it |
| `/apps/NAME` | requests, 5xx and p50/p95 per minute for the last hour, instances (uptime, node, logs), releases (commit link, who, build time, deploy log, the one a rollback goes to), "Copy command" for deploy, scale and env |
| `/jobs/ID` | output (live while it runs), duration and exit status of a named job's last 50 runs |
| `/nodes/ID` | CPU load, memory and disk now and over the last hour, its instances and jobs, drain/undrain, the `node pool` command |
| `/sandboxes/ID` | age, CPU and memory, `diff --stat`, a macOS sandbox's screen (every 10 s) |
| `/disk` | one bar split into images, containers, volumes and build cache, the data disk on the Mac, what the prunes would free, the five largest images and volumes; compact, clear build cache |
| `/vm` | uptime, kernel, dockerd and API, file sharing and its request rates, sockets, the sandbox VM |
| `/activity`, `/stream` | see Actions |

Traffic comes from the edges: each counts requests, 5xx and latency per app per minute and
reports finished minutes to the controller (`POST /routes/activity`), which keeps 24 hours.
With several edges the counts add up and the percentiles are the worst edge's.

Notifications (the bell) are opt-in and per kind: a release fails, an instance is lost, a
node stops checking in, a job fails, a container exits non-zero. Each event notifies once;
what was already wrong when the page loaded doesn't. The settings live in `localStorage`.

Rows are keyboard-reachable: Tab or the arrow keys move, Enter opens, `.` opens the row's
menu, `x` ticks it; Escape clears the selection. `/` focuses the filter, 1-9 open the list
pages (in the order containers, images, volumes, apps, jobs, sandboxes, projects, ports,
fleet), and `g` then a letter goes to any page: `g h` overview, `g c` containers, `g i`
images, `g v` volumes, `g a` apps, `g j` jobs, `g s` sandboxes, `g p` projects, `g o` ports,
`g f` fleet, `g d` disk, `g m` VM, `g y` activity. `?` lists all of them.

The log view numbers lines and colours warn and error ones. Search is plain text or a regex
(the `.*` button); it counts matches, Enter and Shift+Enter move between them, "Only
matching" hides the rest and the copy button copies the lines shown. "Next error" jumps to
the next error line. Only the lines near the view are rendered, wrapped or not: wrapped ones
are measured as they appear and the rest count as one line until they do. Each new state is structurally shared with the last (`lib/share.ts`), so
rows whose data didn't change keep their objects and the memoised ones skip rendering. At phone width tables drop secondary
columns and every row action is in the row's menu.

## Tests

```sh
npm test           # vitest: allow-list, issues, notifications, palette (pages, recents), stats parsing, state merge,
                   # route validation, share, sorting, log levels and search, bulk kinds, activity filters, 5xx rate,
                   # the token and cookie, guard.ts (Host, Origin, Sec-Fetch-Site, x-rekcod), the stream cap
npm run test:e2e   # Playwright against the fixture bundle, run through start.cjs and signed in through /auth
                   # (auth.setup.ts); 401 without the cookie and for a wrong token (auth.spec.ts); every list and detail page, axe (no
                   # serious violations, no hydration errors), /?tab= redirect, Cmd+B rail, 404, filter, confirm/cancel, dry-run toast, /stream,
                   # keyboard rows, Cmd+K, 390 px, the phone drawer (dashboard.spec.ts); sorting, bulk select and its
                   # confirm, container tabs, log search, shortcuts, palette, empty states, toast View, row flash,
                   # activity filters and Run again, charts, fleet tabs, disk, pins, density, title (round4.spec.ts)
```

Both run in CI (`dashboard` job) and in `.githooks/pre-commit` when a staged file is under
`dashboard/`.

## Limitations

- The token keeps out other machines and containers, not other processes running as you:
  they can read `dashboard.json` or the server's environment.
- Jobs come from the controller's last 50; the menu bar shows 10.
- One remote at a time: the default one, like the menu bar. `remote use` and `ns use` switch it.
- Deploys, scaling, env changes and `node pool` take arguments, so they're CLI-only; their
  pages show the command to copy.
- Activity, the stats history and node history live in the server's memory: restarting it
  clears them. Node history only grows while a page is open (the server reads state then).
- Pins, density, the palette's recents and the sidebar fold are per browser (`localStorage`).
- The seventh container name onwards reuses a colour (six slots plus grey for "other").
- `rekcod state` still calls `/apps` once for the action lists and once with `?detail=1` for
  the rest; an older controller ignores `detail` and gets one call per app.
