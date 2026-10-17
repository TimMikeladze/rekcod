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
- Automatic login on (System Settings › Users & Groups). LaunchAgents only run in a logged-in
  session, so without it nothing comes back after a reboot. FileVault blocks automatic login.

rekcod itself does **not** have to be installed there: `host add` copies this Mac's binary over.
No SSH? [server-pkg.md](server-pkg.md) builds a pkg you double-click on the server instead.

## What `host add --server` does

1. Checks SSH works without a password; if it only takes one, installs rekcod's key with
   `ssh-copy-id` first.
2. Finds `rekcod` on the server (login shell `PATH`, then `~/.rekcod/bin/rekcod`). If there's
   none, copies this binary to `~/.rekcod/bin/rekcod` with scp (beside it, then renamed, so
   agents running the old one survive). A dev build also copies the local guest to
   `~/.rekcod/guest` when its `VERSION` differs.
3. Runs `rekcod server setup --domain D` there over `ssh -t`, so sudo can ask for a password.
4. Reads the `remote URL TOKEN` line the setup prints and runs `rekcod remote add NAME URL`
   here. The server's tailnet name goes in the URL, so it survives an IP change.
5. Without `--no-docker`: runs `rekcod start` on the server, then the plain `host add` steps
   (docker context `rekcod-NAME`, switched to unless `--no-use`).

Re-running it is safe: every step replaces what it made before.

## `rekcod server setup` (on the server)

The Rust version of the old setup shell script, plus the headless bits.

- Address: `--addr`, else `tailscale ip -4`, else (or with `--lan`) none: RustFS and the node
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
- Warns when automatic login is off (`autoLoginUser` in `com.apple.loginwindow`).
- Last line on stdout: `remote https://NAME:7700 ADMIN_TOKEN CA-FINGERPRINT`, with NAME
  `rekcod-<fingerprint>.bonjour` on a LAN.

`rekcod server uninstall` removes the four agents. It leaves pmset and `~/.rekcod` alone.

## Not done

- Turning on automatic login, Remote Login or Tailscale. They need the GUI, an Apple ID or the
  user's password in a form I'd rather not pass around.
- cloudflared. The setup prints the commands; public traffic stays opt-in.
- Menu bar: `host add` takes arguments the menu has no way to ask for, same as before.
- Starting the docker VM at login. After a reboot apps come back (they're sandboxes the node
  starts), but docker over SSH needs `rekcod start` on the server first.
