// Fake answers for the read-only routes in fixture mode (REKCOD_STATE_FILE), so the sheets
// have something to show without a VM or a fleet.
import { deflateSync } from "node:zlib"
import type { Container, Job, State } from "@/lib/types"

export function fakeInspect(c: Container) {
  const started = new Date(Date.now() - 3 * 3600_000).toISOString()
  return {
    Id: c.id,
    Name: `/${c.name}`,
    Created: new Date(Date.now() - 26 * 3600_000).toISOString(),
    RestartCount: c.name === "redis" ? 2 : 0,
    Path: "docker-entrypoint.sh",
    Args: c.image.startsWith("postgres") ? ["postgres"] : ["npm", "run", "start"],
    Config: {
      Image: c.image,
      Cmd: c.image.startsWith("postgres") ? ["postgres"] : ["npm", "run", "start"],
      Entrypoint: ["docker-entrypoint.sh"],
      WorkingDir: "/app",
      Env: ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "NODE_ENV=development", "DATABASE_URL=postgres://shop:hunter2@pg:5432/shop", "API_TOKEN=sk_test_51Hx9"],
      Labels: {
        ...(c.project ? { "com.docker.compose.project": c.project, "com.docker.compose.service": c.service ?? "" } : {}),
        "org.opencontainers.image.source": "https://github.com/acme/shop",
      },
      Healthcheck: { Test: ["CMD-SHELL", "curl -fs localhost:3000/health || exit 1"], Interval: 10e9 },
    },
    State: {
      Status: c.state,
      Running: c.state === "running",
      StartedAt: started,
      FinishedAt: c.state === "running" ? "0001-01-01T00:00:00Z" : new Date(Date.now() - 3600_000).toISOString(),
      ExitCode: c.state === "running" ? 0 : 137,
      Health:
        c.state === "running"
          ? { Status: "healthy", FailingStreak: 0, Log: [{ Start: started, End: started, ExitCode: 0, Output: '{"ok":true}\n' }] }
          : undefined,
    },
    Mounts: c.volumes.map((v) => ({ Type: "volume", Name: v, Source: `/var/lib/docker/volumes/${v}/_data`, Destination: "/data", RW: true })).concat(
      c.project ? [{ Type: "bind", Name: "", Source: "/Users/me/shop", Destination: "/app", RW: true }] : [],
    ),
    NetworkSettings: {
      Networks: { [c.project ? `${c.project}_default` : "bridge"]: { IPAddress: "172.18.0.3", Gateway: "172.18.0.1", Aliases: c.service ? [c.service] : null } },
    },
    HostConfig: { RestartPolicy: { Name: c.project ? "unless-stopped" : "no" } },
  }
}

const LOG = [
  "GET /health 200 2ms",
  "GET /api/products 200 18ms",
  "POST /api/cart 201 31ms",
  "GET /api/products/42 404 4ms",
  "warn: slow query (412ms): select * from orders where ...",
  "GET /static/app.js 304 1ms",
  "error: upstream timed out (redis:6379)",
]

/** One log line for second `i` before now (or at now). */
export function fakeLogLine(i: number, timestamps: boolean) {
  const t = new Date(Date.now() - i * 2000).toISOString()
  const line = LOG[Math.abs(Math.floor(Date.now() / 2000) - i) % LOG.length]
  return timestamps ? `${t} ${line}` : line
}

export function fakeLogs(timestamps: boolean, n = 200) {
  return Array.from({ length: n }, (_, k) => fakeLogLine(n - k, timestamps))
}

export function fakeRelease(app: string, n: number, s: State) {
  const r = s.apps.find((a) => a.name === app)?.detail?.releases.find((x) => x.n === n)
  const failed = r?.state === "failed"
  const log = [
    `release ${n} of ${app} at ${r?.commit ?? "0000000"}, by ${r?.who ?? "tim"}`,
    "placing 2 instance(s) on spare, mini",
    "spare: started sb-1a2b at 100.64.0.2:40101 (built, image stored)",
    "mini: started sb-3c4d at 100.64.0.3:40102",
    ...(failed
      ? ["sb-3c4d: health check / failed within 30s; last log lines:\nError: Cannot find module './VERSION'", "failed; the previous release keeps serving"]
      : ["sb-1a2b: / healthy after 3.2s", "sb-3c4d: / healthy after 3.9s", "live; draining 2 old instance(s) for 30s"]),
  ].join("\n")
  return { app, n, commit: r?.commit ?? "", state: r?.state ?? "old", created: r?.created ?? 0, who: r?.who ?? "tim", finished: r?.finished ?? null, log }
}

export function fakeJobHistory(name: string, s: State): { jobs: Job[]; schedules: [] } {
  const now = s.generated
  const jobs = Array.from({ length: 20 }, (_, i) => {
    const created = now - (i + 1) * 86_400
    const took = 40 + ((i * 37) % 90)
    const exit = i % 7 === 3 ? 1 : 0
    return {
      id: `j-fake${String(i).padStart(2, "0")}`,
      name,
      state: exit ? "failed" : "done",
      exit,
      created,
      finished: created + took,
      cmd: ["python", "etl.py"],
      node: i % 2 ? "mini" : "spare",
      owner: "tim",
      schedule: name,
      actions: [],
    }
  })
  return { jobs, schedules: [] }
}

export const FAKE_DIFF_STAT = ` src/cart.ts        | 42 +++++++++++++++++++++++++-----
 src/checkout.tsx   | 17 ++++++------
 package.json       |  2 +-
 3 files changed, 44 insertions(+), 17 deletions(-)
`

// A small PNG: a gradient with a bar, so the thumbnail visibly changes between refreshes.
function crc32(b: Buffer) {
  let c = ~0
  for (const x of b) {
    c ^= x
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1))
  }
  return ~c >>> 0
}

function chunk(type: string, data: Buffer) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const td = Buffer.concat([Buffer.from(type), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(td))
  return Buffer.concat([len, td, crc])
}

export function fakePng(w = 160, h = 100): Buffer {
  const shift = Math.floor(Date.now() / 10_000) % w
  const raw = Buffer.alloc((w * 3 + 1) * h)
  for (let y = 0; y < h; y++) {
    raw[y * (w * 3 + 1)] = 0
    for (let x = 0; x < w; x++) {
      const o = y * (w * 3 + 1) + 1 + x * 3
      const bar = y > 10 && y < 18 && x > shift && x < shift + 30
      raw[o] = bar ? 240 : 30 + (x * 120) / w
      raw[o + 1] = bar ? 240 : 40 + (y * 80) / h
      raw[o + 2] = bar ? 240 : 90
    }
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(w, 0)
  ihdr.writeUInt32BE(h, 4)
  ihdr[8] = 8
  ihdr[9] = 2
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))])
}
