# Remote docker hosts

Use the rekcod VM on another Mac as your docker engine, over SSH. This is plain
docker-over-SSH (`ssh://` contexts). It's separate from the fleet (`rekcod remote`, apps),
which deploys apps instead of handing out a docker socket. `host add --server` sets up both at
once ([server.md](server.md)).

## What the user runs

On the other Mac: `rekcod start`, and turn on Remote Login. On yours:

```sh
rekcod host add mini you@mac-mini     # or you@host:2222; checks SSH, sets up that Mac, creates + switches context
docker ps                             # runs on that Mac's VM
rekcod host forward mini              # its published ports on this Mac's localhost (Ctrl-C stops)
rekcod host use local                 # back to this Mac's rekcod
```

## Pieces

- `rekcod host add NAME DEST [--no-use]`
  1. `ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new DEST true` (records the host key on first use). docker's ssh
     helper can't answer a password prompt, so on `Permission denied` in a terminal it installs
     rekcod's own key with `ssh-copy-id` and an `~/.ssh/config` Include ([ssh-keys.md](ssh-keys.md)),
     then checks again. `host rm` drops the key's config block.
  2. Finds a docker CLI on that Mac: `command -v docker` in a login shell, else rekcod's
     bundled one under `~/.rekcod/cli/`.
  3. Writes a marked block to that Mac's `~/.zshenv`, replacing an older one. It applies only
     to non-interactive SSH sessions, which is what docker's helper uses:
     `PATH` gets the docker CLI's directory, and `DOCKER_HOST` points at
     `~/.rekcod/run/docker.sock`. Non-login SSH on macOS has no `/opt/homebrew/bin`, and the
     Mac's active context can change (`rekcod stop` switches it back), so this pins both.
  4. Creates or updates the docker context `rekcod-NAME` (`host=ssh://DEST`), checks
     `docker --context rekcod-NAME version` reaches the engine, then switches to it.
- Hosts are the docker contexts named `rekcod-*`. No other state is stored.
- `rekcod host ls [--json]`, `rekcod host use NAME|local`, `rekcod host rm NAME`.
- `rekcod host forward NAME [PORT...]` runs in the foreground: one SSH master connection,
  then every 2s it lists its published TCP ports (`docker ps` over that connection) and
  adds or cancels `-L 127.0.0.1:P:localhost:P` forwards. With ports given, only those. A port
  already taken on this Mac is reported once and skipped.

## Menu bar

A "Docker host" submenu, shown even when the local VM is stopped: "Use <name>" / "Use local
rekcod" (`Kind::HostUse`) for the contexts not in use, and "Forward ports from <name>"
(`Kind::HostForward`, runs in Terminal). A server set up with `host add --server --lan` also gets
"Find <name> again on the LAN" (`Kind::HostRefresh`, `rekcod host refresh NAME`), which
re-resolves its Bonjour name and updates the context once the Mac there proves it has the
SSH host key pinned at `add`; `host use` and `host forward` do that
first anyway ([lan-discovery.md](lan-discovery.md)).

## Limitations

- The other Mac's login shell must be zsh (the macOS default); otherwise `add` stops and prints the
  lines to add by hand.
- Bind mounts refer to paths on the other Mac. Builds send their context over SSH.
