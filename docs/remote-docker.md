# Remote docker hosts

Use the rekcod VM on another Mac (a spare) as your docker engine, over SSH. This is plain
docker-over-SSH (`ssh://` contexts). It's separate from the fleet (`rekcod remote`, apps),
which deploys apps instead of handing out a docker socket.

## What the user runs

On the spare: `rekcod start`, and turn on Remote Login. On the dev Mac:

```sh
rekcod host add spare you@spare-mac   # or you@host:2222; checks SSH, sets up the spare, creates + switches context
docker ps                             # runs on the spare's VM
rekcod host forward spare             # its published ports on this Mac's localhost (Ctrl-C stops)
rekcod host use local                 # back to this Mac's rekcod
```

## Pieces

- `rekcod host add NAME DEST [--no-use]`
  1. `ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new DEST true` (records the host key on first use). Fails with an `ssh-copy-id` hint, since docker's ssh
     helper can't answer a password prompt.
  2. Finds a docker CLI on the spare: `command -v docker` in a login shell, else rekcod's
     bundled one under `~/.rekcod/cli/`.
  3. Writes a marked block to the spare's `~/.zshenv`, replacing an older one. It applies only
     to non-interactive SSH sessions, which is what docker's helper uses:
     `PATH` gets the docker CLI's directory, and `DOCKER_HOST` points at
     `~/.rekcod/run/docker.sock`. Non-login SSH on macOS has no `/opt/homebrew/bin`, and the
     spare's active context can change (`rekcod stop` switches it back), so this pins both.
  4. Creates or updates the docker context `rekcod-NAME` (`host=ssh://DEST`), checks
     `docker --context rekcod-NAME version` reaches the engine, then switches to it.
- Hosts are the docker contexts named `rekcod-*`. No other state is stored.
- `rekcod host ls [--json]`, `rekcod host use NAME|local`, `rekcod host rm NAME`.
- `rekcod host forward NAME [PORT...]` runs in the foreground: one SSH master connection,
  then every 2s it lists the spare's published TCP ports (`docker ps` over that connection) and
  adds or cancels `-L 127.0.0.1:P:localhost:P` forwards. With ports given, only those. A port
  already taken on this Mac is reported once and skipped.

## Menu bar

A "Docker host" submenu, shown even when the local VM is stopped: "Use <name>" / "Use local
rekcod" (`Kind::HostUse`) for the contexts not in use, and "Forward ports from <name>"
(`Kind::HostForward`, runs in Terminal).

## Limitations

- The spare's login shell must be zsh (the macOS default); otherwise `add` stops and prints the
  lines to add by hand.
- Bind mounts refer to the spare's paths. Builds send their context over SSH.
