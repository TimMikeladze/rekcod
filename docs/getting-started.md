# Getting started

From nothing to your compose project running on rekcod, then the rekcod-only features in the
order you'll likely reach for them. Needs an Apple Silicon Mac.

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/TimMikeladze/rekcod/main/scripts/install.sh | bash
```

The script checks the release tarball's sha256 and installs nothing on a mismatch. It puts
`rekcod` in `/usr/local/bin` and asks for sudo only if `/usr/local` isn't writable. Manual
steps and Homebrew: [README, Install](../README.md#install).

## 2. Start the VM

```sh
rekcod start
```

The first run downloads and verifies the guest image (~100 MB). After that it boots in about
1.3 s. It creates the `rekcod` docker context and selects it, so plain `docker` now talks to
rekcod. The first `rekcod start` in a terminal also asks for your password once, to route
`/var/run/docker.sock` to rekcod for tools that ignore contexts (SDKs, IDE plugins).

Sizing is optional and saved in `~/.rekcod/config.json`:

```sh
rekcod start --cpus 8 --memory 16 --disk 512   # disk only grows, never shrinks
```

No docker CLI installed? Skip it. `rekcod ps`, `rekcod run`, `rekcod compose` work as the
docker CLI and download docker, compose and buildx to `~/.rekcod/cli` on first use.

## 3. Bring your images and volumes

```sh
rekcod import --from desktop-linux   # Docker Desktop
rekcod import --from orbstack        # OrbStack
```

Images and named volumes come across, so compose reuses your existing database volumes.
Volumes in use at the source are skipped unless you pass `--force`. `--images-only` and
`--volumes-only` narrow it.

## 4. Run your project

Nothing changes in your compose files:

```sh
cd ~/code/myapp
docker compose up -d
docker compose logs -f
```

Published ports show up on the Mac's `localhost`. If one is already taken on the Mac, rekcod
leaves the other process alone and lists the port in `rekcod status`. Bind mounts under
`/Users`, `/Volumes`, `/private`, `/tmp` and `/var/folders` work at the same paths, and hot
reload works.

## 5. Keep `node_modules` out of macOS

Add a label and those directories live only in the VM. Installs get faster and Mac and Linux
native builds never mix:

```yaml
services:
  web:
    build: .
    volumes: [.:/app]
    working_dir: /app
    labels:
      dev.rekcod.local: "node_modules,.next"   # relative to working_dir, or absolute
```

The Mac sees an empty directory there. `docker volume rm rekcod-local_<project>_<service>_<path>`
starts it over.

## 6. Snapshot your database

```sh
rekcod snap save seeded      # from the project directory; pauses its containers, copies volumes
rekcod snap restore seeded   # back to that state in about a second
rekcod snap save             # no name: named after the current git branch
rekcod snap ls
```

A bad restore is undone with `rekcod snap restore pre-restore`.

## 7. Run tools without a Dockerfile

```sh
rekcod x npm install && rekcod x npm run dev   # node image matching .nvmrc / package.json
rekcod x uv run pytest
rekcod x cargo test
rekcod x --sandbox npm install sketchy-pkg     # throwaway VM, sees only this directory
```

`rekcod x` picks the image from the command and your version files, mounts the current
directory at the same path, and forwards whatever ports the command listens on. `--sandbox`
runs it in a separate VM with no access to the rest of `$HOME`, the Mac or the local network.
Details: [sandbox.md](sandbox.md).

## 8. Menu bar and dashboard

```sh
rekcod menubar --install     # menu bar item, starts at login
rekcod dashboard             # web UI on http://localhost:4500 or the next free port
```

Both offer the same actions as the CLI: start/stop containers, logs, open ports, snapshots,
prune images and volumes. Anything destructive asks first.

## Day to day

```sh
rekcod status          # versions, ports (and any that couldn't bind), disk use
rekcod shell           # root shell in the VM
rekcod compact         # give deleted image/volume space back to macOS now (also hourly)
rekcod start --install # boot at login and restart if it stops unexpectedly
rekcod stop            # power off, switch back to your previous docker context
```

`rekcod stop` deletes `--rm` containers, like a reboot would.

## When something's wrong

```sh
rekcod doctor            # checks VM, versions, disk, sockets, context, internet; prints a fix for each
rekcod doctor --bundle   # tarball of logs to attach to an issue
rekcod stop --force      # hung VM
rekcod start --repair    # VM won't boot because the data disk has errors
```

Behind a full-tunnel VPN (Mullvad and others) the VM switches to `--network user` on its own.
More: [troubleshooting.md](troubleshooting.md).

## Upgrading and removing

```sh
rekcod upgrade                     # latest release, checksum-verified
rekcod stop && rekcod start        # the VM runs the old version until it restarts
rekcod uninstall [--keep-data]     # removes agents, socket route, context, and ~/.rekcod unless kept
```

## Going further

- Agent sandboxes with an HTTP API and MCP server: [agent-sandboxes.md](agent-sandboxes.md)
- Another Mac as your docker engine: [remote-docker.md](remote-docker.md)
- Self-host apps on a server Mac: [self-hosting.md](self-hosting.md)
- App hosting across several Macs: [fleet-compute.md](fleet-compute.md), [namespaces.md](namespaces.md)
- Local S3 with RustFS: [rustfs.md](rustfs.md)
- Every command: [README, Commands](../README.md#commands)
