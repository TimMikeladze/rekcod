# Installing, upgrading and removing rekcod

How rekcod gets onto a Mac, moves to a new version, tells you what's wrong, and comes off
again. Items 7 and 8 of `docs/production-readiness.md`.

## Upgrade

`rekcod upgrade` asks GitHub for the latest release
(`api.github.com/repos/TimMikeladze/rekcod/releases/latest`, no auth, no identifying
headers), compares it with its own version, and if it's newer downloads
`rekcod-darwin-arm64.tar.gz` and `SHA256SUMS` from the release, checks the tarball's sha256,
and replaces its own binary (written next to it, then renamed over it, so a running VM
daemon keeps the old file). The dashboard bundle in the tarball replaces
`../share/rekcod/dashboard` next to the binary the same way.

- `--check` only says whether there's a newer version (exit 1 if there is).
- A binary under a Homebrew Cellar refuses and says `brew upgrade rekcod`.
- A binary it can't write (root-owned `/usr/local/bin`) asks to be run with sudo.
- The VM keeps running the old version. `rekcod start` already says when the running VM
  is from another version; `rekcod stop && rekcod start` switches it.
- `REKCOD_RELEASE_URL` points the download elsewhere, as for the guest.

## Version handshake

Every fleet request carries `X-Rekcod-Version`. The controller answers 400 when the client's
`major.minor` differs from its own (0.x versions treat minor as major), naming both, so an
old CLI or node gets told to upgrade instead of failing on a changed API. Requests without
the header (curl, older clients) pass. `server setup` copies its binary to the server when the
server's `rekcod --version` differs, not only when it has none.

## LaunchAgents and Homebrew

LaunchAgents record the path the binary was run from when that path is a symlink into a
Homebrew Cellar (`/opt/homebrew/bin/rekcod`), not the Cellar path itself, which changes on
every `brew upgrade`. `packaging/homebrew/rekcod.rb` is the formula for a tap; it isn't
published yet.

## Uninstall

`rekcod uninstall` undoes everything rekcod set up, in order:

1. stops the VM, the `--sandbox` VM and every agent sandbox;
2. removes its launchd jobs (`vm`, `menubar`, `dashboard`, `controller`, `node`, `edge`,
   `storage`), LaunchDaemons too (sudo);
3. hands `/var/run/docker.sock` back (`socket uninstall`, sudo asks once) when rekcod had it;
4. switches the docker context back and removes the `rekcod` context;
5. on a Mac that `server setup` or `host add` changed: removes the `~/.zshenv` block, the
   application firewall's exceptions for rekcod and RustFS, and puts `pmset` back as setup
   found it (recorded in `~/.rekcod/server/pmset-before.json`; a server set up before that
   was recorded only gets `disablesleep 0`), sudo for the last two;
6. deletes `~/.rekcod` (VM disks, snapshots, sandboxes, macOS images, logs) and the
   `Include` rekcod added to `~/.ssh/config`;
7. prints the binary's path for you to delete (it may be root-owned or brew-managed).

`--keep-data` stops after step 5. It asks before deleting anything unless `--yes`. The menu
bar and dashboard offer it with a confirmation; it runs in Terminal because of sudo.

### Another Mac in your fleet

`rekcod host uninstall NAME`, run on your Mac, asks first (`--yes` doesn't), then:

1. copies this build there if it differs, so it knows everything there is to undo;
2. runs `rekcod uninstall --yes` there over `ssh -t` (sudo asks for that Mac's password) and
   deletes the binary in `~/.rekcod/bin`;
3. removes rekcod's key from that Mac's `~/.ssh/authorized_keys`, last, since rekcod can't
   SSH in after that;
4. here: removes the `rekcod-NAME` docker context and its SSH config block, and
   `remote rm NAME` if there's a remote by that name.

If step 2 fails nothing is removed here. A pkg install's `/usr/local/bin/rekcod` is
root-owned and stays; uninstall prints its path.

## Diagnostics

`rekcod doctor` prints checks with a fix for each that fails: VM running and answering,
daemon and guest versions against the CLI, free disk on the Mac, data disk errors in the
console log, sockets and `run/` permissions, docker context, `DOCKER_HOST`, the default
socket, VM internet reachability (one TCP connect from a throwaway container), login
agent state. `--bundle` also writes `~/.rekcod/doctor-<time>.tar.gz` with `status --json`,
the last 2000 lines of each log and the config, never tokens or keys.

## Logs

Each boot starts fresh `daemon.log` and `console.log` and keeps the previous boot's as
`.1`. Long-running processes (the VM daemon, controller, node, edge, storage, menu bar)
check their own log hourly and, past 20 MiB, copy it to `.1` and truncate it. Their stderr
is opened `O_APPEND`, so writes carry on at the new end.
