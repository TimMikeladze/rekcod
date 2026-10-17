# Self-hosting on a server Mac

Turn another Mac into a server that hosts your apps and doubles as a remote docker engine. You
run one command from your own Mac; after that the server sits with its lid shut. The full
reference for what gets installed is [server.md](server.md).

## 1. Prepare the server (by hand, once)

These need the server's screen or your Apple ID, so rekcod doesn't do them:

1. System Settings › General › Sharing › **Remote Login** on.
2. Optional: Tailscale, logged in to the same tailnet as your Mac. Then the fleet uses the
   server's tailnet address and works from anywhere on the tailnet. Without it, see
   [On a LAN without Tailscale](#on-a-lan-without-tailscale).
3. System Settings › Users & Groups › **Automatic login** on. The services are LaunchAgents,
   which only run in a logged-in session, so without this nothing comes back after a reboot.
   FileVault blocks automatic login.

rekcod does **not** need to be installed on the server.

To skip SSH altogether, AirDrop a pkg to the server and double-click it instead of steps 2
and 3: [server-pkg.md](server-pkg.md).

## 2. SSH

Nothing to do. If `ssh me@mac-mini` already gets in without a password, step 3 uses that.
Otherwise step 3 asks for the password on the server once and installs rekcod's own key
(`~/.rekcod/ssh/id_ed25519`, [ssh-keys.md](ssh-keys.md)).

## 3. Provision it

```sh
rekcod host add mini me@mac-mini --server --domain apps.example.com
```

What happens:

1. Copies your `rekcod` binary to `~/.rekcod/bin/rekcod` on the server if it has none. Use a
   release build: a dev build has no embedded guest, and the server's `rekcod start` fails.
2. Runs `rekcod server setup` there, which installs four LaunchAgents: RustFS storage
   (`:7710`), the controller (`:7700`), a node (`:7701`) and the edge (`:8080`, public apps on
   `127.0.0.1:8081`). With Tailscale they listen on its address; without, see below.
3. Runs `sudo pmset -a sleep 0 disablesleep 1 autorestart 1 womp 1` so the lid can shut and
   a power cut doesn't leave it off. sudo asks for the server's password. `--no-power` skips it.
4. Adds the remote `mini` on your Mac with the admin token.
5. Starts the server's docker VM and adds the docker context `rekcod-mini`, switching to it.
   `--no-docker` skips this, `--no-use` adds the context without switching.

Re-running the command is safe; every step replaces what it made before.

Check it:

```sh
rekcod remote ls
rekcod node ls --remote mini
```

## On a LAN without Tailscale

The server's LAN IP and its name can both change, so nothing is tied to either. When the
server has no Tailscale address (or you pass `--lan`):

- RustFS and the node listen on the server's `127.0.0.1`. The controller and edge listen on
  every interface.
- The controller advertises itself over Bonjour as `rekcod-<first 12 hex of its CA
  fingerprint>`. The remote on your Mac is `https://rekcod-3f9a1c2b7d4e.bonjour`, resolved
  on every call, and TLS is still checked against the pinned CA. A server with a new IP or
  name is found again; another machine on its old IP fails the CA check.
- The docker context remembers the Bonjour name and the server's SSH host key. `rekcod host
  use mini` and `rekcod host forward mini` re-resolve the name first and switch only if the
  Mac they find proves it has that host key. `rekcod host refresh mini` does only that.

```sh
rekcod host add mini me@mac-mini --server --domain apps.example.com --lan
rekcod remote ls     # * mini  https://rekcod-3f9a1c2b7d4e.bonjour  ns=default
```

Limits: mDNS stays on one network segment, so your Mac and the server need the same subnet
(no VLAN or guest network between them). Private apps are served on `:8080` to the whole LAN.
To open one in a browser you still need the server's current address, such as
`http://<server's name>.local:8080` with the app's Host header; a public app through
cloudflared doesn't depend on it. `--addr IP` still pins a single address if you'd rather
reserve one on your router. Design: [lan-discovery.md](lan-discovery.md).

## 4. Deploy an app

From your app's repo. The server builds the pushed `HEAD` of `origin`, so push first and make
sure the server can clone that URL.

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
rekcod app deploy --remote mini
rekcod app status blog --remote mini
rekcod app logs blog --remote mini
```

With a `Dockerfile` it's built as is. Without one, rekcod picks a toolchain image from your
version files like `rekcod x` does and runs `start`.

The app answers on `http://<server's tailnet name>:8080` with `Host: blog.apps.example.com`.
Point a wildcard DNS record `*.apps.example.com` at the server's tailnet IP to open it in a
browser from any machine on your tailnet.

Day to day:

```sh
rekcod app env set blog KEY=value --remote mini    # redeploys the live release
rekcod app secret set blog STRIPE_KEY=sk_live_... --remote mini
rekcod app rollback blog --remote mini
rekcod remote use mini                             # then drop --remote
```

## 5. Make an app public (optional)

Apps are private (tailnet only) until you say otherwise:

```sh
rekcod app public blog --remote mini
```

Public traffic reaches the server through a Cloudflare tunnel pointed at `127.0.0.1:8081`.
`server setup` prints the `cloudflared` commands at the end; rekcod doesn't install
cloudflared for you. The ingress rule looks like
`ingress: [{hostname: "*.example.com", service: http://127.0.0.1:8081}]`.

The edge rate-limits each client IP to 50 requests/s and bans IPs that keep hitting the
limit. `rekcod edge ban 203.0.113.7 --for 1h` bans one by hand. More: [edge-http.md](edge-http.md).

## 6. Use it as a docker engine

Step 3 already switched your docker context to the server:

```sh
docker ps                    # runs on the server
rekcod host forward mini     # its published ports on 127.0.0.1 here, until Ctrl-C
rekcod host use local        # back to this Mac's rekcod
```

Bind mounts are paths on the server, not your Mac. More: [remote-docker.md](remote-docker.md).

## Backups

The controller's state is `~/.rekcod/controller/fleet.db` on the server. With storage set up
(as `server setup` does) it's backed up to RustFS every hour; `rekcod controller backups --remote mini`
lists them. Copy `~/.rekcod/controller/secret-key` off the server somewhere safe: app secrets
are sealed with it and can't be read without it.

## After a reboot

Apps come back on their own. Docker over SSH doesn't: run `ssh me@mac-mini rekcod start`
first, or `rekcod start --install` on the server to boot the VM at login.

## Removing it

```sh
ssh me@mac-mini '~/.rekcod/bin/rekcod server uninstall'   # removes the four agents
rekcod remote rm mini
rekcod host rm mini
```

`server uninstall` leaves the pmset settings and `~/.rekcod` on the server alone.
