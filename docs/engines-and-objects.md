# Engines, images, volumes and networks from every front end

What the dashboard and menu bar were missing next to Docker Desktop: pulling and running
images, creating volumes and networks, and managing another Mac's rekcod from this one.
Every item below is a CLI command first, then an `actions.rs` kind, then a menu bar item
and a dashboard control.

## Remote engines

A host (`rekcod host add`) is another Mac running rekcod. Two commands make it manageable
from here:

- `rekcod host state NAME` runs `rekcod state --local` there over SSH and prints it with
  every action rewritten to run there. The rewrite is the whole trick: the remote binary
  builds its own actions, so every kind it knows works remotely without new code here.
- `rekcod host exec NAME [--dir DIR] -- ARGS` runs `rekcod ARGS` there (`ssh -t` when
  stdin is a terminal). Rewritten actions are `host exec NAME -- <their argv>`.

The rewrite drops actions that only make sense on the Mac they were built for: URL opens
(their `localhost` is the other Mac's), the dashboard, uninstall, host/remote/namespace
switching, screenshots that open a viewer there. `host forward` stays the way to reach a
remote port from here.

rekcod is found on the remote the same way `host restart` finds it (login shell, then
`~/.rekcod/bin`). An older rekcod there just offers fewer kinds.

### Dashboard

One engine at a time per dashboard server, picked in the top bar (`local` or a host).
`POST /api/engine` switches it; the hub drops its cache and restarts the events and stats
followers with the `host exec` prefix. Fleet keys and `hosts` always come from the local
state. The allow-list check (`offered`) reads the selected engine's fresh state merged with
the local one, so only argv either side offers runs.

### Menu bar

Each host gets a submenu with its VM state, containers, images, volumes and networks, built
from `host state` on the background loader (SSH timeouts never block the menu).

## Actions that take input

`Action` gets `inputs`: each has a `key`, label, `kind` (image, name, publish, env, mount),
whether it's optional or repeatable, and an optional flag. The argv holds `{key}` where the
values go:

- a whole-token `{key}` with a flag becomes `flag value` per value, or nothing when empty
- a whole-token `{key}` without a flag becomes the value(s)
- `{key}` inside a token (`{target}:/to`) is replaced by the single required value

Values are validated by kind (no leading `-`, no newlines, shape per kind) in Rust
(`actions::fill`, used by the menu bar) and TypeScript (`lib/inputs.ts`, used by
`/api/run`). `/api/run` takes `{argv, values}`: `argv` must equal an offered template
exactly, then it's filled. The menu bar asks with an `NSAlert` text field per input.

## New kinds

| Kind | Command | Where |
| --- | --- | --- |
| ImagePull | `pull {image}` | engine actions; per tagged image ("Pull again") |
| ImageRun | `run -d [--name] [-p]… [-e]… [-v]… IMAGE` | engine actions (`{image}`); per image |
| ImageTag | `tag IMAGE {target}` | per image |
| ImagePush | `push TAG` (asks first) | per tagged image |
| ImageHistory | `image history IMAGE` (streamed) | per image |
| ContainerPause / ContainerUnpause | `pause` / `unpause` | per container |
| ContainerShell | `exec -it NAME sh` (Terminal) | per running container |
| VolumeCreate | `volume create {name}` | engine actions |
| VolumeClone | helper container `cp -a` into `{target}` | per volume |
| VolumeExport | helper container `tar` to `~/Downloads/NAME.tar.gz` | per volume |
| VolumeBrowse | helper container `find` (streamed) | per volume |
| NetworkCreate | `network create {name}` | engine actions |
| NetworkRemove | `network rm` (asks first) | per user-created network |
| NetworkPrune / SystemPrune | `network prune -f` / `system prune -f` | disk actions |

Networks come from `GET /networks`, with the containers on each from the container list.
`bridge`, `host` and `none` have no actions.
