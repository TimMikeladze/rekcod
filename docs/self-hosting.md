# Self-hosting on a spare Mac

Turn a spare Mac into a box that hosts your apps and doubles as a remote docker engine. You
run one command from your own Mac; after that the spare sits with its lid shut. The full
reference for what gets installed is [spare.md](spare.md).

## 1. Prepare the spare (by hand, once)

These need the spare's screen or your Apple ID, so rekcod doesn't do them:

1. System Settings › General › Sharing › **Remote Login** on.
2. Optional: Tailscale, logged in to the same tailnet as your Mac. Then the fleet uses the
   spare's tailnet address and works from anywhere on the tailnet. Without it, see
   [On a LAN without Tailscale](#on-a-lan-without-tailscale).
3. System Settings › Users & Groups › **Automatic login** on. The services are LaunchAgents,
   which only run in a logged-in session, so without this nothing comes back after a reboot.
   FileVault blocks automatic login.

rekcod does **not** need to be installed on the spare.

## 2. Set up SSH from your Mac

```sh
ssh-copy-id me@spare-mac
ssh me@spare-mac true     # must not ask for a password
```

## 3. Provision it

```sh
rekcod host add spare me@spare-mac --fleet --domain apps.example.com
```

What happens:

1. Copies your `rekcod` binary to `~/.rekcod/bin/rekcod` on the spare if it has none. Use a
   release build: a dev build has no embedded guest, and the spare's `rekcod start` fails.
2. Runs `rekcod spare setup` there, which installs four LaunchAgents: RustFS storage
   (`:7710`), the controller (`:7700`), a node (`:7701`) and the edge (`:8080`, public apps on
   `127.0.0.1:8081`). With Tailscale they listen on its address; without, see below.
3. Runs `sudo pmset -a sleep 0 disablesleep 1 autorestart 1 womp 1` so the lid can shut and
   a power cut doesn't leave it off. sudo asks for the spare's password. `--no-power` skips it.
4. Adds the remote `spare` on your Mac with the admin token.
5. Starts the spare's docker VM and adds the docker context `rekcod-spare`, switching to it.
   `--no-docker` skips this, `--no-use` adds the context without switching.

Re-running the command is safe; every step replaces what it made before.

Check it:

```sh
rekcod remote ls
rekcod node ls --remote spare
```

## On a LAN without Tailscale

The spare's LAN IP and its name can both change, so nothing is tied to either. When the
spare has no Tailscale address (or you pass `--lan`):

- RustFS and the node listen on the spare's `127.0.0.1`. The controller and edge listen on
  every interface.
- The controller advertises itself over Bonjour as `rekcod-<first 12 hex of its CA
  fingerprint>`. The remote on your Mac is `https://rekcod-3f9a1c2b7d4e.bonjour`, resolved
  on every call, and TLS is still checked against the pinned CA. A spare with a new IP or
  name is found again; another machine on its old IP fails the CA check.
- The docker context remembers the Bonjour name and the spare's SSH host key. `rekcod host
  use spare` and `rekcod host forward spare` re-resolve the name first and switch only if the
  Mac they find proves it has that host key. `rekcod host refresh spare` does only that.

```sh
rekcod host add spare me@spare-mac --fleet --domain apps.example.com --lan
rekcod remote ls     # * spare  https://rekcod-3f9a1c2b7d4e.bonjour  ns=default
```

Limits: mDNS stays on one network segment, so your Mac and the spare need the same subnet
(no VLAN or guest network between them). Private apps are served on `:8080` to the whole LAN.
To open one in a browser you still need the spare's current address, such as
`http://<spare's name>.local:8080` with the app's Host header; a public app through
cloudflared doesn't depend on it. `--addr IP` still pins a single address if you'd rather
reserve one on your router. Design: [lan-discovery.md](lan-discovery.md).

## 4. Deploy an app

From your app's repo. The spare builds the pushed `HEAD` of `origin`, so push first and make
sure the spare can clone that URL.

```toml
# rekcod.toml, all optional
[app]
name = "blog"
port = 3000            # defaults to 3000
start = "npm start"    # only used when there's no Dockerfile
health = "/healthz"    # must answer 200 within 30 s or the release fails

[resources]
memory = "1GiB"
idle_stop = "15m"      # sleep when idle, wake on the next request

[env]
NODE_ENV = "production"
```

```sh
git push
rekcod app deploy --remote spare
rekcod app status blog --remote spare
rekcod app logs blog --remote spare
```

With a `Dockerfile` it's built as is. Without one, rekcod picks a toolchain image from your
version files like `rekcod x` does and runs `start`.

The app answers on `http://<spare's tailnet name>:8080` with `Host: blog.apps.example.com`.
Point a wildcard DNS record `*.apps.example.com` at the spare's tailnet IP to open it in a
browser from any machine on your tailnet.

Day to day:

```sh
rekcod app env set blog KEY=value --remote spare   # redeploys the live release
rekcod app secret set blog STRIPE_KEY=sk_live_... --remote spare
rekcod app rollback blog --remote spare
rekcod remote use spare                            # then drop --remote
```

## 5. Make an app public (optional)

Apps are private (tailnet only) until you say otherwise:

```sh
rekcod app public blog --remote spare
```

Public traffic reaches the spare through a Cloudflare tunnel pointed at `127.0.0.1:8081`.
`spare setup` prints the `cloudflared` commands at the end; rekcod doesn't install
cloudflared for you. The ingress rule looks like
`ingress: [{hostname: "*.example.com", service: http://127.0.0.1:8081}]`.

The edge rate-limits each client IP to 50 requests/s and bans IPs that keep hitting the
limit. `rekcod edge ban 203.0.113.7 --for 1h` bans one by hand. More: [edge-http.md](edge-http.md).

## 6. Use it as a docker engine

Step 3 already switched your docker context to the spare:

```sh
docker ps                    # runs on the spare
rekcod host forward spare    # its published ports on 127.0.0.1 here, until Ctrl-C
rekcod host use local        # back to this Mac's rekcod
```

Bind mounts are paths on the spare, not your Mac. More: [remote-docker.md](remote-docker.md).

## Backups

The controller's state is `~/.rekcod/controller/fleet.db` on the spare. With storage set up
(as `spare setup` does) it's backed up to RustFS every hour; `rekcod controller backups --remote spare`
lists them. Copy `~/.rekcod/controller/secret-key` off the spare somewhere safe: app secrets
are sealed with it and can't be read without it.

## After a reboot

Apps come back on their own. Docker over SSH doesn't: run `ssh me@spare-mac rekcod start`
first, or `rekcod start --install` on the spare to boot the VM at login.

## Removing it

```sh
ssh me@spare-mac '~/.rekcod/bin/rekcod spare uninstall'   # removes the four agents
rekcod remote rm spare
rekcod host rm spare
```

`spare uninstall` leaves the pmset settings and `~/.rekcod` on the spare alone.
