# Two edges

Run an edge on two Macs that can reach the same controller and app nodes. Provision an
additional Mac with [edge-only server setup](server.md#add-another-mac-to-an-existing-fleet),
or run `rekcod edge serve` separately with a restricted edge remote on each Mac.

Each edge fetches the same routes and bans. Traffic and client counts add together at the
controller; reported latency percentiles keep the worst edge's value. Rate limits and
strike counters remain per edge. Finished minutes are reported every two seconds, while
an unfinished minute stays in memory and is lost if that edge exits.
Reports are best effort; failed activity reports are not retried.

## Public apps through Cloudflare Tunnel

Run a connector of the **same tunnel** on each edge Mac, using the existing tunnel UUID
and credentials. Each connector points to its own Mac's edge. For a locally managed tunnel,
put this in each Mac's `~/.cloudflared/config.yml`, with the credentials path for that Mac:

```yaml
tunnel: EXISTING_TUNNEL_UUID
credentials-file: /path/to/EXISTING_TUNNEL_UUID.json
ingress:
  - hostname: "*.apps.example.com"
    service: http://127.0.0.1:8081
  - service: http_status:404
```

Run `cloudflared tunnel run EXISTING_TUNNEL_NAME` on both Macs. Keep the existing DNS route
to that tunnel. Only apps marked public are available on port 8081.

[Cloudflare's replica documentation](https://developers.cloudflare.com/tunnel/configuration/#replicas-and-high-availability)
describes continuing through the remaining connectors when a host goes offline. If only the
edge process fails while its connector stays connected, use an ingress with origin health
checks or supervise the connector together with the edge. Connector redundancy alone
does not establish that the local edge is healthy. Existing connections to a failed Mac close.

## Tailnet clients

Configure a load balancer with both edges' tailnet addresses on port 8080 and health checks,
or configure clients to retry another address after a connection failure. Preserve the
app's Host header. Two DNS addresses alone do not guarantee that a client retries a request.

## Verify the second edge

```sh
curl -H 'Host: web.apps.example.com' http://EDGE_A:8080/
curl -H 'Host: web.apps.example.com' http://EDGE_B:8080/
```

Both must reach the same deployed app before connecting the public ingress. Stopping one
edge tests the ingress or client's failover policy; the other edge continues serving from
its own route table. This does not add controller failover. If the controller is down,
cached routes keep serving awake apps, while deploys, wakes and automatic recovery stop.

`python3 scripts/e2e-edges.py` starts two edge processes, a controller and a loopback backend
in temporary homes. It checks edge-only setup, restricted tokens, summed metrics and client
counts, then terminates one edge during 100 requests through a client that retries the
survivor. It installs no launchd jobs and changes no power or firewall settings. It does
not connect to Cloudflare or verify an external ingress configuration.
