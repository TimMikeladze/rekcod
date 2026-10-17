# A Mac as a server

One command on your Mac turns another Mac into a server: a deploy target and a docker host.
After that it runs with its lid shut and nobody touches it.

```sh
rekcod host add mini me@mac-mini --server --domain apps.example.com
```

## What the server needs first

Done once, by hand, because they need its screen or an Apple ID:

- Remote Login on (System Settings › General › Sharing). `host add` puts a key on it the first
  time, asking for the password once ([ssh-keys.md](ssh-keys.md)).
- Tailscale logged in, on the same tailnet as your Mac. Without it the fleet goes on the LAN
  and your Mac finds it over Bonjour, whatever its IP or name ([lan-discovery.md](lan-discovery.md));
  `--lan` picks that even with Tailscale. `--addr IP` pins one address you manage instead.
- An admin account you can `sudo` with. With it the services start at boot, nobody logged
  in (below); without, they wait for someone to log in.

rekcod itself does **not** have to be installed there: `host add` copies this Mac's binary over.
No SSH? [server-pkg.md](server-pkg.md) builds a pkg you double-click on the server instead.

## What `host add --server` does

1. Checks SSH works without a password; if it only takes one, installs rekcod's key with
   `ssh-copy-id` first.
2. Finds `rekcod` on the server (login shell `PATH`, then `~/.rekcod/bin/rekcod`). If there's
   none, copies this binary to `~/.rekcod/bin/rekcod` with scp (beside it, then renamed, so
   agents running the old one survive). A copy in `~/.rekcod/bin` is replaced whenever its
   sha256 differs, since dev builds share a version. A binary without the virtualization
   entitlement (a plain `cargo build`) is sent as a signed copy. A dev build also copies the local guest to
   `~/.rekcod/guest` when its `VERSION` differs.
3. Runs `rekcod server setup --domain D` there over `ssh -t`, so sudo can ask for a password.
4. Reads the `remote URL TOKEN` line the setup prints and runs `rekcod remote add NAME URL`
   here. The server's tailnet name goes in the URL, so it survives an IP change.
5. Without `--no-docker`: runs `rekcod start` on the server and `rekcod version` (which
   downloads a docker CLI when it has none), then the plain `host add` steps
   (docker context `rekcod-NAME`, switched to unless `--no-use`).

Re-running it is safe: every step replaces what it made before.

## `rekcod server setup` (on the server)

The Rust version of the old setup shell script, plus the headless bits.

- Address: `--addr`, else `tailscale ip -4` if that IP is on one of the Mac's interfaces
  (a stopped Tailscale still prints its old one), else (or with `--lan`) none: RustFS and the node
  on 127.0.0.1, the controller and edge on 0.0.0.0, the controller on Bonjour (`--bonjour`).
  A node that joined on another address is removed (`node rm`) and joins again.
- `storage serve --install` on `ADDR:7710` (RustFS, [`rustfs.md`](rustfs.md)), waits for its
  keys in `storage/credentials.json`.
- `controller serve --storage http://ADDR:7710 --install` on `ADDR:7700`, waits for the admin token.
- `node join ... --install` on `ADDR:7701` the first time, `node serve --install` after.
- `remote add local`, then `edge serve --install` on `ADDR:8080` and public apps on
  `127.0.0.1:8081` for cloudflared.
- Power, unless `--no-power`: `sudo pmset -a sleep 0 disablesleep 1 autorestart 1 womp 1`.
  `disablesleep` keeps a MacBook awake with the lid shut, `autorestart` boots it after a power
  cut. Skipped with a warning when sudo can't prompt (no terminal).
- Firewall: when the application firewall is on and rekcod isn't in its list,
  `sudo socketfilterfw --add` and `--unblockapp` for the binary. Without them other Macs'
  connections to the controller and edge are accepted and then get nothing back. Without a
  terminal for sudo it prints the command instead.
- `start --install`: the docker VM, as a job like the others.
- Without sudo, after the agents: prints `sudo rekcod server at-boot`, and when automatic
  login is off says what's in the way (FileVault, or opens Users & Groups on the screen).
  rekcod doesn't switch automatic login on itself: that takes the user's password.
- Last line on stdout: `remote https://NAME:7700 ADMIN_TOKEN CA-FINGERPRINT`, with NAME
  `rekcod-<fingerprint>.bonjour` on a LAN.

`rekcod server uninstall` removes the five jobs (sudo for daemons). It leaves pmset and
`~/.rekcod` alone. To take rekcod off the server entirely, from your Mac:
`rekcod host uninstall NAME` ([install-upgrade.md](install-upgrade.md#another-mac-in-your-fleet)).

## Add another Mac to an existing fleet

On the additional Mac, configure the existing controller's admin remote and join token:

```sh
rekcod remote add fleet https://controller:7700 --token ADMIN_TOKEN --ca-fingerprint sha256:...
rekcod server setup --edge-only --remote fleet --join-token JOIN_TOKEN
```

This installs a node and an edge. It uses the existing controller and storage; the additional
Mac gets no controller, RustFS or main Docker VM. Its node starts sandbox VMs as needed.
The edge uses a node-specific restricted token. The configured admin remote stays available
for setup and administration, separate from the edge service's remote.

Address selection is `--addr IP`, then Tailscale, then the LAN. On a LAN the node listens
on `0.0.0.0:7701` and advertises the Mac's Sharing hostname with `.local`; the controller must
be able to resolve it. The edge listens on port 8080, and public apps on `127.0.0.1:8081`.
If the controller requires approval, run `rekcod node approve NODE_ID --remote fleet` from
an admin client using the ID setup prints.

Repeat the setup command without `--join-token` once joined. It preserves the node ID.
A node joined with another controller or address causes an error before service changes;
use the individual node commands to change that configuration. The existing `--no-power`
and boot service behavior apply to edge-only setup too.

For traffic failover, connect both edges to an ingress that can use a surviving Mac.
[Two edges](fleet-edges.md) covers Cloudflare Tunnel and tailnet clients.

## Starting at boot

LaunchAgents run in a login session, so a server built from them comes back after a reboot
only once someone logs in, and macOS won't log in automatically while FileVault is on. The
services don't need a login: they're network servers and VMs. So `server setup` installs
them as LaunchDaemons whenever it can get root (`sudo -v`, which asks in a terminal):

- `/Library/LaunchDaemons/dev.rekcod.{storage,controller,node,edge,vm}.plist`, root:wheel
  0644, loaded into launchd's `system` domain.
- `UserName` is you and `GroupName` staff, so they run as you, with your `~/.rekcod`, as the
  agents did. launchd gives a daemon no login environment, so the plist sets `HOME`, `USER`
  and a `PATH`.
- A service that has a daemon stays one: a later `--install` of it rewrites the daemon (sudo
  again), and its LaunchAgent, if any, is removed so the two never run side by side.
  `REKCOD_LAUNCHD=system` makes new ones daemons, which is how `server setup` tells the
  commands it runs.

Without sudo (no terminal, as in the pkg's postinstall) setup falls back to LaunchAgents,
which still need someone logged in on the screen. `rekcod server at-boot` turns them into
daemons later: it re-runs itself under sudo, reads each agent's command line and log back
with `plutil`, installs the daemon and removes the agent. As root it takes `--user NAME`,
which is how the pkg uses it.

FileVault still holds a cold boot at its unlock screen until someone types the password
there; nothing on the disk runs before that. For a restart you plan, run
`rekcod host restart NAME` on your Mac (or `rekcod server restart` on the server). It asks
for the server account's password once, checks it with `sudo -v`, checks that the account
is in `fdesetup list`, then runs `fdesetup authrestart -inputplist` with the account and
password on stdin, so the disk unlocks for that one boot. Asked directly, fdesetup wants a
password, a user name and a password again, unlabelled. Without FileVault it's
`shutdown -r now`. `host restart` then waits for a new `kern.boottime` (a Mac can go down and
come back between two polls, so "it stopped answering" proves nothing) and for its docker
engine to answer, and says how long that took. I haven't found a way around an unplanned
one that doesn't mean turning FileVault off.

## Not done

- Turning on Remote Login or Tailscale. They need the GUI or an Apple ID.
- cloudflared. The setup prints the commands; public traffic stays opt-in.
- Menu bar: `host add` takes arguments the menu has no way to ask for, same as before.
- Starting the docker VM at login. After a reboot apps come back (they're sandboxes the node
  starts), but docker over SSH needs `rekcod start` on the server first.
