# Server installer (.pkg)

`host add --server` needs SSH to the server before anything works. The installer skips that:
build `rekcod-server-X.Y.Z.pkg` on your Mac, AirDrop it to the server, double-click it there.

## Building it

```sh
scripts/package-server.sh                          # out/server/rekcod-server-0.3.0.pkg
SERVER_DOMAIN=apps.example.com scripts/package-server.sh
INSTALLER_IDENTITY="Developer ID Installer: ..." scripts/package-server.sh   # signed pkg
```

It runs `scripts/package.sh` (release binary with the guest version embedded), then wraps
the binary and `out/guest` in a pkg. The guest goes in `/usr/local/share/rekcod/guest`,
which `Paths::guest_dir` checks before downloading, so the guest needn't be on a GitHub release.
The server still needs the internet once: `storage serve` downloads RustFS.

`packaging/server/test.sh` runs postinstall against a stub `rekcod` and stubbed system
commands, as you and through the root branch, and checks the pairing line, its file mode and
the log. CI runs it.

## What the pkg installs

| path | what |
|---|---|
| `/usr/local/bin/rekcod` | the release binary |
| `/usr/local/share/rekcod/guest/` | `Image`, `root.img`, `VERSION` |
| `/usr/local/share/rekcod/server-domain` | `SERVER_DOMAIN`, default `apps.localhost` |

## What postinstall does

Installer runs it as root. The services are LaunchAgents, so they need the user logged in on
the server's screen; it finds them with `stat -f %Su /dev/console`.

1. `pmset -a sleep 0 disablesleep 1 autorestart 1 womp 1` (root already, so `server setup`
   gets `--no-power`).
2. As that user (`launchctl asuser UID sudo -u USER`): `rekcod server setup --domain D
   --no-power`. Tailscale if it's up, otherwise the LAN and Bonjour, as in
   [server.md](server.md).
3. As that user: `rekcod start --install`, so the docker VM runs at login.
4. Writes `~/Desktop/rekcod-pair.txt` (0600) with `rekcod remote add NAME ...` built from
   setup's last line. NAME is the Mac's LocalHostName in lower case: `Tims-Mac-mini` becomes
   `tims-mac-mini`. It copies that line to the clipboard (Universal Clipboard puts it on
   your Mac if both share an Apple ID) and opens the file in TextEdit. When privacy settings
   keep the installer out of `~/Desktop`, the file goes in `~/rekcod-pair.txt` instead.

Nobody logged in on the console (the login window, or Setup Assistant's `_mbsetupuser`): it
installs the files, runs 1, skips 2 to 4 and says so. A setup failure fails the install and
opens `/var/log/rekcod-server-install.log`. The log is 0600 and belongs to that user. Setup
hides tokens in the commands it reports, and its last line, the one with the admin token,
never reaches the log.

Re-installing is safe: setup replaces its agents, which restarts them on the new binary.

## Pairing from your Mac

Paste the line from `rekcod-pair.txt` on your Mac, then `rekcod dashboard`. The dashboard
shows the server's apps, nodes and jobs from that remote. Delete `rekcod-pair.txt`
afterwards: it holds the controller's admin token.

Docker on the server still goes over SSH. Turn on Remote Login there, then on your Mac:
`rekcod host add tims-mac-mini me@tims-mac-mini.local` (no `--server`, the fleet's already set
up). It asks for the password there once to install its key.

## Not done

- Signing and notarizing the pkg needs a "Developer ID Installer" certificate, which there
  isn't yet. Unsigned, the server refuses it on the first double-click; System Settings ›
  Privacy & Security › Open Anyway lets it through.
- Turning on Remote Login and automatic login. Both are system settings I'd rather an
  installer didn't flip quietly. setup warns when automatic login is off.
- The server's own web dashboard needs Node, so the pkg doesn't install it.
