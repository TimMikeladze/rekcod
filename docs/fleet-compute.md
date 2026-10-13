# Fleet compute: scale to zero, recycling, jobs

**Status:** built 2026-10-12, checked by `scripts/e2e-fleet.sh`. Builds on tracks 2 and 3
in [`platform.md`](platform.md).

Virtualization.framework keeps every page a guest has touched (`phase5.md` item 4), so a VM
gives memory back to macOS only when it stops. Two of the three pieces here stop VMs on
purpose. The third, jobs, runs work that is supposed to finish.

## Scale to zero

`idle_stop = "15m"` under `[resources]` in `rekcod.toml` (or `app deploy --idle-stop 15m`).
Default is off, `0`. PR previews get 15 minutes unless the base app sets something.

- The edge records the last request per host and posts the hosts it saw to
  `POST /routes/activity` every 2 s. The controller stores `apps.last_request`.
- The controller's watchdog stops every live instance of an app whose last request and live
  release are both older than `idle_stop`. Instance state goes `live -> sleeping` first, so
  the route disappears before the VM stops; then the node stops the sandbox VM
  (`POST /sandboxes/ID/stop`). The VM disk stays, so nothing is rebuilt.
- `GET /routes` lists a sleeping app with no endpoints and `"sleeping": true`.
- A request for a sleeping app: the edge holds the connection and calls
  `POST /apps/NAME/wake`. The controller takes the app's lock (concurrent requests share one
  wake), asks each node to `POST /instances/ID/wake`, which boots the VM and `docker start`s
  the app container, waits for the health check (30 s), marks the instances live and returns
  their endpoints. The edge forwards the held request.
- Wake failure (node lost, VM won't boot): the controller redeploys the live release's
  commit instead. That's a rebuild, so it's slow, but the request still gets an answer if it
  finishes inside the edge's 120 s hold. Otherwise 503.
- Health checks come from the controller straight to the instance, not through the edge, so
  they never count as activity.
- `rekcod app wake NAME` wakes it by hand; `app status` shows `sleeping`.

What stops when an app sleeps: everything in it. Background loops, queue consumers and
open WebSockets die. Only HTTP through the edge wakes it.

## Recycling

`recycle = "24h"` under `[resources]` (or `--recycle 24h`). Default off.

- The watchdog redeploys the live release's commit when it's older than `recycle`. That is
  the normal zero-downtime deploy (new instances, health, switch, drain), so the new VMs
  start with only what the app touches, and the old VMs' memory goes back to macOS.
- `rekcod app recycle NAME` does the same now.
- A sleeping app isn't recycled; waking it is already a fresh VM process.
- It rebuilds, because there's no registry yet (see `rustfs.md`). For a big image that's a
  real cost every `recycle` period, so pick hours, not minutes.

## Jobs

A job is a command that runs to completion in a fresh sandbox on the fleet, then the sandbox
is deleted.

```sh
rekcod job run [--name etl] [--image python:3.13-slim] [--cpus 4] [--memory 16] \
  [--env K=V]... [--timeout 1h] [--every 1h] [--git URL --ref REF] -- python etl.py
rekcod job ls                 # recent jobs and schedules
rekcod job logs ID            # output (last 64 KiB once finished)
rekcod job cancel ID
rekcod job unschedule NAME
```

- The workspace is this repo's pushed HEAD (or `--git/--ref`), cloned on the node, like
  `sandbox --remote create`. The image defaults to the `rekcod x` detection from version
  files, else `alpine:3.22`. There's no install step: put it in the command
  (`sh -c "pip install -r requirements.txt && python etl.py"`).
- Controller places the job (`memory` GiB free), the node creates the sandbox, runs the
  command in a background thread writing `~/.rekcod/jobs/ID/{log,result.json}`, then removes
  the sandbox. `--timeout` kills it with exit 124.
- The controller polls running jobs every 5 s (`GET /jobs/ID` on the node), and on finish
  stores exit code, duration and the log's last 64 KiB in `fleet.db`. Polling means a
  controller restart doesn't lose jobs.
- `--every 1h` stores a schedule instead of running once. The watchdog starts a run when the
  previous one finished and `every` has passed since it started. A schedule never runs two
  at once.
- A job on a node that goes silent is marked `lost`.
- Owners and quotas: jobs belong to the token that started them, like sandboxes. Running
  jobs count toward `--max-sandboxes`.

## Menu bar

Per `CLAUDE.md`, each new action is a `Kind`:
- apps: `AppRecycle` ("Restart instances"), `AppWake` (only when sleeping);
- a Jobs submenu on the default remote: `JobLogs`, `JobCancel` (running only, confirms).

## Verification

`scripts/e2e-fleet.sh` gains:
- an app with `idle_stop` 5 s goes to `sleeping`, its VM stops, the next request returns 200
  through the edge, and `app status` shows it live again;
- `app recycle` gives new instances with no non-200s;
- `job run -- sh -c "echo hi; exit 3"` reports exit 3 and `hi` in `job logs`;
  a `--timeout 2s` job exits 124; `job cancel` stops a sleeper; bob can't read alice's job;
  an `--every 3s` schedule runs at least twice.
