# Troubleshooting

Start with `rekcod doctor`. It checks the things below and prints a fix for each one that
fails; `rekcod doctor --bundle` writes a tarball of status and logs (no tokens or keys) to
attach to an issue.

Logs live in `~/.rekcod/logs`: `daemon.log` (the Mac side), `console.log` (the guest's
kernel and boot), each with the previous boot's copy as `.1`. Inside the VM,
`rekcod shell` and `/var/lib/docker/dockerd.log`.

## The VM keeps memory after a big job

`rekcod status` has a `memory` line when the VM has elastic memory; that part goes back to
macOS 5 to 15 s after the guest frees it. Base RAM stays until `rekcod stop`. No `memory`
line: the Mac is on macOS before 27, `--memory` is 4 GiB or less, or the guest image is
older than elastic memory (`rekcod upgrade`). A smaller `--base-memory` moves more into
elastic memory. See [elastic-memory.md](elastic-memory.md).

## amd64 containers fail with `exec format error`

Rosetta isn't installed on the Mac (`rosetta=false` in `daemon.log`). Run
`softwareupdate --install-rosetta --agree-to-license`, then `rekcod stop && rekcod start`.
See [rosetta.md](rosetta.md).

## The VM won't start

- `the VM's data disk can't be used: ...`: the data disk isn't readable ext4. rekcod formats
  a data disk only on the boots after it created `vm/data.img` and before dockerd first
  answered on it (`vm/data.fresh`), and only while its first 4 MiB are zeros. Any other
  disk is left as it is, also one that is zeros at the start: damage there leaves the data
  behind it. `rekcod stop`, then `rekcod start --repair` runs `e2fsck -fy` on it (trying
  every backup superblock) and reinstalls the root disk. If that fails too, copy
  `~/.rekcod/vm/data.img` off if anything on it matters, then move it out of `vm/`; the
  next `rekcod start` creates an empty disk and formats that one.
- `dockerd did not answer within 120s`: see the end of `console.log`. A guest that stops
  during boot usually says why there.
- `another rekcod daemon is running`: one is, for that `REKCOD_HOME`. `rekcod status`
  shows its pid; `rekcod stop --force` powers off a hung one.

## `rekcod doctor` says the state isn't private

`~/.rekcod` and its `run/`, `logs/` and `vm/` are 0700, and the VM disks (`vm/root.img`,
`vm/data.img`) 0600, since the data disk holds every volume and container. Every `rekcod`
command sets those modes, whatever the umask, also on an install made before it did. A
failed check means it couldn't, because the path belongs to another account (root, after a
`sudo rekcod ...`): `sudo chown -R "$(whoami)" ~/.rekcod`, and the next `rekcod` command
sets the modes.

## A docker stream ends with an error

`docker logs`, `export`, `cp` and `save` end with an error, never a short clean EOF, when
the stream from the VM breaks: the vsock connection failed (its error is passed on), a
stalled reader's spill to a temp file failed (`couldn't spill a stalled stream`), or the
reader fell more than 16 GiB behind on top of the 64 MiB kept in memory (`reader fell too
far behind`). Both limits count unread bytes, so a long stream whose reader keeps up is
never cut. Spill files are unlinked files in `$TMPDIR`, closed (and their space freed) as
they're read.

## Containers can't reach the internet

DNS working while connections time out is Virtualization.framework's NAT being blocked,
usually by a VPN or firewall app that filters the `bridge100`/vmnet traffic (Mullvad does,
with its default settings). `rekcod doctor` tests a connection from a throwaway container
and says which network the VM uses. `rekcod stop && rekcod start --network user` makes
rekcod open the VM's connections itself, from the Mac, so the VPN carries them like any
app's. `--network auto` (the default) does that by itself when the default route goes
through a tunnel at boot; a VPN connected after the VM started needs a restart. DNS for VPN-only names goes
through the Mac's resolvers, including per-domain ones (`scutil --dns`).

## A port isn't on localhost

`rekcod status` lists every forwarded port; one that couldn't be bound shows the error,
usually that another process on the Mac has it (rekcod never takes a port from another
process, including one listening on `*:PORT`). Stop that process, then restart the
container.

## A container sees an old version of a file

Bind-mounted files are cached in the guest and invalidated from FSEvents, typically within
15 ms. If FSEvents drops events (a very large burst), rekcod invalidates whole directories
and retries until the guest takes them. `REKCOD_DEBUG=1` in the daemon's environment logs
every forwarded event to `daemon.log`.

## The menu bar shows stale state

It reloads every 15 s and when opened, waiting at most 1.5 s for a fresh load; with a slow
or unreachable fleet controller it shows the last state it had. `rekcod state --local`
skips the controller.

## After upgrading

`rekcod start` says when the running VM is from another version; `rekcod stop && rekcod
start` switches it. Fleet commands against a controller on another minor version fail with
both versions named: upgrade the older side.
