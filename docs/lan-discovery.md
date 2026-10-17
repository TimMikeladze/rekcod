# A server Mac on a LAN without fixed addresses

`server setup --addr IP` baked one LAN IP into every listener, the controller's certificate,
`node.json` and the remote on your Mac. On a home network the IP and the Mac's name both
change, and then nothing connects. Tailscale avoids that with a stable 100.x address and
MagicDNS name. This is the same guarantee without Tailscale.

## Identity, not address

The controller's CA fingerprint already identifies the server: remotes pin it. With `--lan`
the controller advertises itself over Bonjour as `rekcod-<first 12 hex of the fingerprint>`,
type `_rekcod._tcp`. The remote on your Mac is

```
https://rekcod-3f9a1c2b7d4e.bonjour
```

`.bonjour` isn't DNS. Every call resolves the instance with `DNSServiceResolve` (mDNSResponder,
no extra dependency) to its current host name and port, then connects. The controller's
certificate includes that name, so TLS is checked against the pinned CA and that name as for
any remote. A renamed or re-addressed server is found again, and something else on the LAN
answering on the old IP fails the CA check.

mDNS doesn't cross subnets or VLANs. The server and your Mac must be on the same segment.

## Nothing on the server uses its LAN address

| Service | Tailscale / `--addr` | `--lan` |
|---|---|---|
| RustFS | `IP:7710` | `127.0.0.1:7710` |
| Controller | `IP:7700` | `0.0.0.0:7700`, Bonjour |
| Node | `IP:7701`, advertised as `https://IP:7701` | `127.0.0.1:7701` |
| Edge | `IP:8080` | `0.0.0.0:8080` |
| Remotes `local`, `edge` on the server | `https://IP:7700` | `https://127.0.0.1:7700` |

App instances need S3 (`S3_ENDPOINT`), and an instance VM can't reach the Mac's loopback. When
the node gets a loopback storage endpoint, it hands the app `http://198.18.0.1:7710` instead and
lets the VM reach exactly that address. The netstack dials `127.0.0.1:7710` for it.
198.18.0.0/15 is the benchmarking range, never routed, so it can't be anything real.

`--lan` is used when Tailscale has no address and `--addr` isn't given. `--addr` keeps its
old meaning, a fixed IP you manage.

## Docker over SSH

The docker context is `ssh://user@host`, which docker resolves itself. `host add --server`
on a LAN server reads the server's `/etc/ssh/ssh_host_ed25519_key.pub` over the SSH session it
just authenticated and stores it, with the Bonjour instance, in the context's description.
`rekcod host refresh [NAME]` resolves the instance again and connects to whatever answered
with `StrictHostKeyChecking=yes` and a known_hosts holding only that pinned key. Only if that
succeeds does it write the key for the new name to `~/.ssh/known_hosts` (docker's ssh can't
answer a prompt) and update the context. `host use` and `host forward` refresh first.

The CA fingerprint can't be the proof here: Bonjour advertises its prefix and any client can
fetch the CA, so an impostor's SSH server could hand it back. The host key it can't fake.
A context added before host keys were pinned has no `hostkey=` and is never refreshed;
re-run `host add` to pin one.

## Not covered

- Browsing apps on `:8080` from your Mac still needs an address for the server. Use the Bonjour
  name the controller prints (`<name>.local`), or a public app through cloudflared, which
  doesn't care about the LAN IP.
- A multi-Mac fleet with `--lan` storage: other nodes can't reach the server's loopback RustFS.
  `--lan` is for the one-machine server.
