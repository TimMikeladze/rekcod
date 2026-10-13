# Edge HTTP on hyper

`rekcod edge serve` (`crates/rekcod/src/edge/`) is a tokio + hyper reverse proxy. It parses
every request, routes it on its own Host, balances across an app's instances, rate limits per
client IP and drops banned addresses.

## What the old edge got wrong

The old edge ran a thread per connection on `std::net::TcpStream`. It read the head of the
first request, picked a backend by Host and then piped bytes both ways. The problems:

- Routing happened once per connection. cloudflared pools upstream connections across
  hostnames, so on `--public-listen` a request for a private app could ride a connection
  opened for a public one and reach it.
- Round robin only applied to a connection's first request, and a dead instance cost a
  connect timeout every time.
- There was no client IP to limit or ban on. Behind a tunnel the peer is the tunnel.

## Crates

| Crate | Used for |
| --- | --- |
| `hyper` 1, `hyper-util` (`server-auto`) | HTTP/1.1 and h2c server, HTTP/1.1 client connections to instances |
| `http-body-util` | body boxing, `Limited` for the body cap |
| `governor` | keyed token buckets |
| `ipnet` | CIDR bans |

I left out `tower::balance`. Its `Balance` service has no way to retry another instance
without consuming the request body, and that retry is the whole point of skipping dead
instances. `balance.rs` is a small P2C instead. PROXY v2 parsing is 20 lines in
`client_ip.rs`, so there's no crate for it either. TLS is out of scope, since the tunnel in
front of `--public-listen` terminates it.

## Request path

```
accept (one tokio task per connection, at most 10 000)
  -> PROXY v2 header, if --proxy-protocol            client_ip.rs
  -> banned peer? close without reading anything     ban.rs
  -> hyper auto (h1 / h2c), header read timeout 10 s, closed after 90 s idle   idle.rs
  -> per request:
       client IP = CF-Connecting-IP / X-Forwarded-For if the peer is a trusted proxy, else the peer
       banned client IP? 403
       Host -> Route (2 s refresh from GET /routes; the public listener only sees public apps)
       edge-wide and per-app buckets             limit.rs   -> 429 + Retry-After
       Content-Length over max_body              -> 413 (a chunked body: cut at the limit, then 413)
       no endpoints? wake the app (POST /ns/NS/apps/APP/wake), as before
       lease a connection                        balance.rs -> 503 when none answers
       send, 60 s for the response headers       -> 504 / 502
       --access-log: one stderr line per request
       101? splice client and instance with copy_bidirectional
```

The request sent to the instance is HTTP/1.1 with an origin-form path. Hop-by-hop headers are
stripped and `Host` is kept (from `:authority` for h2 clients). `X-Forwarded-For` is set to
the client IP, or appended to when trusted, with `X-Forwarded-Proto: http` and an
`X-Request-Id` if the client sent none. From an untrusted peer `CF-Connecting-IP` is removed,
so an app can't be fooled by a header the client wrote.

## Load balancing

Each endpoint has an in-flight count, a peak-EWMA of time to response headers (a slow response
counts in full immediately, fast ones decay it 30% at a time), a down-until time and up to 32
idle connections. A lease picks two random endpoints that aren't down and takes the lower
`(ewma + 1) * (in_flight + 1)`. It reuses an idle connection to that endpoint or opens one,
with a 3 s connect timeout. A connect failure marks the endpoint down for 5 s and moves on to
the next. Down endpoints are tried last, not skipped, in case all of them are. Since connecting
happens before the body is touched, any request can move to the next endpoint, not just
idempotent ones. A pooled connection that closes mid-request is a 502, not a retry.

## Rate limits and bans

- Edge-wide bucket per client IP: `--rate 50/s --burst 100` by default.
- Per app, from `rekcod.toml`, stored in `apps.http` and sent along with each route:

  ```toml
  [http]
  rate = "20/s"
  burst = 40          # default: one second's worth
  max_body = "10MB"
  ```

- 100 `429`s from one IP within 60 s ban it here at once for 10 minutes, and the edge sends
  the ban to `POST /bans` so every edge has it after its next refresh.
- Buckets are per edge process. With N edges a client gets N times the rate.
- Idle buckets are pruned on every refresh.

The controller keeps bans in `bans (net, reason, until, created)`, normalized so `1.2.3.4`
and `1.2.3.4/32` are the same row:

| Endpoint (admin token) | Body |
| --- | --- |
| `GET /bans` | unexpired bans |
| `POST /bans` | `{"net", "secs"?, "reason"?}`; replaces an existing ban on the same net |
| `DELETE /bans` | `{"net"}` |

`GET /routes` is unchanged except for the `http` field on each route, so an old edge still
works against a new controller.

CLI: `rekcod edge ban [NET] [--for 1h] [--reason R]` (asks for NET when it's left out, which
is how the menu bar's "Ban an IP or range" works), `edge unban NET`, `edge bans`.

## Flags on `edge serve`

| Flag | Default | |
| --- | --- | --- |
| `--rate`, `--burst` | `50/s`, `100` | edge-wide bucket per client IP |
| `--trusted-proxy NET` | `127.0.0.0/8`, `::1` | peers whose `CF-Connecting-IP` / `X-Forwarded-For` name the client; repeatable, replaces the default |
| `--proxy-protocol` | off | public listener expects a PROXY v2 header on every connection |
| `--access-log` | off | `CLIENT METHOD HOST/PATH STATUS MSms` on stderr per request |

## Trusted proxies

Behind cloudflared every connection comes from the tunnel. Rate limiting or auto-banning that
address would hit every visitor at once, so the client IP comes from the tunnel's headers
whenever the TCP peer (or the PROXY v2 source) is in `--trusted-proxy`. The default is
loopback, which covers cloudflared on the same Mac with no flags. Headers from anyone else are
ignored, since any client can write them. This is the same rule as nginx's `set_real_ip_from`.
It applies to both listeners: a tailnet client isn't loopback, so the private listener keeps
using the peer.

A trusted proxy is never dropped at accept and never auto-banned. Its visitors are checked
for bans on each request and get a `403`. A request from a trusted peer without forwarding
headers, such as `curl` on the same Mac, is limited as the peer. A manual
`edge ban 127.0.0.1` still turns those away.

## Idle connections and body caps

hyper's header read timeout only covers a request head that stalls. `idle.rs` wraps the
socket and records when it last moved a byte. A connection with no request in flight and
nothing read or written for 90 s gets a graceful shutdown. A slow instance doesn't count as
idle, since its request is in flight.

A body with a `Content-Length` over `max_body` is refused before anything reaches the
instance. A chunked one can't be checked up front: it's forwarded until it crosses the limit,
then the upstream request is aborted and the client gets a 413. An instance that answers
before reading the body wins that race, and the client gets the instance's response.

## Tests

`edge::tests` run the real listener against fake instances on loopback:

- keep-alive with two Hosts routes each request
- the public listener 404s a private app on a reused connection
- a dead endpoint is skipped, and an app with only dead endpoints gets a 503
- 429 after the burst, then a ban after 100 strikes
- `X-Forwarded-For` is ignored from untrusted peers; behind a trusted one each visitor has
  their own bucket, and strikes ban the visitor (403), never the tunnel
- a WebSocket-style upgrade pipes bytes
- an idle keep-alive connection is closed, and a chunked body over the cap gets a 413

The unit tests cover PROXY v2 parsing, CIDR normalization, rate and size parsing, and P2C
picking.
