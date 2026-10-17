# Spare Mac on a LAN without fixed addresses

`spare setup --addr IP` baked one LAN IP into every listener, the controller's certificate,
`node.json` and the remote on your Mac. On a home network the IP and the Mac's name both
change, and then nothing connects. Tailscale avoids that with a stable 100.x address and
MagicDNS name. This is the same guarantee without Tailscale.

## Identity, not address

The controller's CA fingerprint already identifies the spare: remotes pin it. With `--lan`
the controller advertises itself over Bonjour as `rekcod-<first 12 hex of the fingerprint>`,
type `_rekcod._tcp`. The remote on your Mac is

```
https://rekcod-3f9a1c2b7d4e.bonjour
```

`.bonjour` isn't DNS. Every call resolves the instance with `DNSServiceResolve` (mDNSResponder,
no extra dependency) to its current host name and port, then connects. The controller's
certificate includes that name, so TLS is checked against the pinned CA and that name as for
any remote. A renamed or re-addressed spare is found again, and something else on the LAN
answering on the old IP fails the CA check.

mDNS doesn't cross subnets or VLANs. The spare and your Mac must be on the same segment.

## Nothing on the spare uses its LAN address

| Service | Tailscale / `--addr` | `--lan` |
|---|---|---|
| RustFS | `IP:7710` | `127.0.0.1:7710` |
| Controller | `IP:7700` | `0.0.0.0:7700`, Bonjour |
| Node | `IP:7701`, advertised as `https://IP:7701` | `127.0.0.1:7701` |
| Edge | `IP:8080` | `0.0.0.0:8080` |
| Remotes `local`, `edge` on the spare | `https://IP:7700` | `https://127.0.0.1:7700` |

App instances need S3 (`S3_ENDPOINT`), and an instance VM can't reach the Mac's loopback. When
the node gets a loopback storage endpoint, it hands the app `http://198.18.0.1:7710` instead and
lets the VM reach exactly that address. The netstack dials `127.0.0.1:7710` for it.
198.18.0.0/15 is the benchmarking range, never routed, so it can't be anything real.

`--lan` is used when Tailscale has no address and `--addr` isn't given. `--addr` keeps its
old meaning, a fixed IP you manage.

## Docker over SSH

The docker context is `ssh://user@host`, which docker resolves itself. `host add --fleet`
on a LAN spare stores the Bonjour instance in the context's description.
`rekcod host refresh [NAME]` resolves it again, checks over SSH that
`~/.rekcod/controller/ca-fingerprint` there is the one it expects, then updates the
context's host. `host use` and `host forward` refresh first.

## Not covered

- Browsing apps on `:8080` from your Mac still needs an address for the spare. Use the Bonjour
  name the controller prints (`<name>.local`), or a public app through cloudflared, which
  doesn't care about the LAN IP.
- A multi-Mac fleet with `--lan` storage: other nodes can't reach the spare's loopback RustFS.
  `--lan` is for the one-machine spare.
