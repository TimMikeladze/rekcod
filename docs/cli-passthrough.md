# rekcod as a drop-in `docker` CLI

Goal: anything you'd type after `docker` works after `rekcod`, with no docker
CLI installed and regardless of the active context.

```sh
rekcod run --rm -it alpine sh
rekcod compose up -d
rekcod ps
rekcod build -t app .
```

## Routing

`rekcod <args>` is a rekcod command when it is one of rekcod's own, else it runs
`docker <args>` against rekcod's socket. Collisions with docker are settled by shape:

| argv | goes to |
|---|---|
| `start` / `stop` with only rekcod flags (`--cpus`, `--memory`, `--disk`, `--no-switch`) | the VM |
| `start` / `stop` with a container name or any other flag (`stop -t 5 web`) | `docker start/stop` |
| `import --from <ctx> ...` | rekcod import |
| `import <file>` (no `--from`) | `docker import` |
| `status`, `shell`, `compact`, `menubar`, `snap`, `x`, `tui`, `help`, `-h`, `--help`, `-V`, `--version`, `daemon`, `research` | rekcod |
| `docker <args>` | always docker (escape hatch, e.g. `rekcod docker start`) |
| anything else | docker |

`rekcod --version` stays rekcod's; `rekcod version` is docker's (client + dockerd).

## Execution

- `exec` replaces the rekcod process, so TTYs, signals and exit codes are docker's own.
- `DOCKER_HOST=unix://~/.rekcod/run/docker.sock`, `DOCKER_CONTEXT` unset: it always
  targets rekcod, whatever context is active.
- If the VM isn't running, it boots first (no context switch), so `rekcod run ...`
  works from cold.

## Which docker CLI

1. `$REKCOD_DOCKER_CLI` if set (a path).
2. `docker` on `PATH` (user's own plugins and config keep working).
3. Bundled: `~/.rekcod/cli/<version>/docker` plus `cli-plugins/docker-compose` and
   `docker-buildx`, downloaded on first use from download.docker.com and the
   docker/compose and docker/buildx GitHub releases, sha256-pinned in `cli_fetch.rs`.
   The bundled CLI runs with `DOCKER_CONFIG=~/.rekcod/docker` (unless the user set
   `DOCKER_CONFIG`) so its plugins are found; `docker login` creds land there.

rekcod's own internal docker calls (contexts, `import`, `shell`) use the same resolution,
so `rekcod import` and `rekcod shell` no longer need a docker CLI either. Context
switching is skipped when only the bundled CLI exists (no one else reads contexts).

Pinned: docker 29.9.0 (matches the guest's dockerd), compose v5.6.0, buildx v0.38.0.
