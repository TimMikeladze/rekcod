import { request } from "node:http"
import { FIXTURE } from "@/lib/rekcod"
import { once } from "@/lib/server/bus"
import { guarded } from "@/lib/server/readonly"

// What answers on each forwarded TCP port: a 1 s HEAD to localhost, cached 10 s.
type Probe = { status: number | null; note: string }
const cache = once("probe", () => new Map<number, { at: number; p: Probe }>())

function head(port: number): Promise<Probe> {
  return new Promise((resolve) => {
    const r = request({ host: "127.0.0.1", port, method: "HEAD", path: "/", timeout: 1000 }, (res) => {
      res.resume()
      resolve({ status: res.statusCode ?? null, note: `HTTP ${res.statusCode}` })
    })
    r.on("timeout", () => {
      r.destroy()
      resolve({ status: null, note: "no answer in 1 s" })
    })
    // A parse error (HPE_*) or a reset means something answered that doesn't speak HTTP.
    r.on("error", (e: NodeJS.ErrnoException) => resolve({ status: null, note: e.code === "ECONNREFUSED" ? "nothing listening" : "not HTTP" }))
    r.end()
  })
}

export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const ports = g.s.ports.filter((p) => !p.udp && !p.error).map((p) => p.port)
  if (FIXTURE) return Response.json(Object.fromEntries(ports.map((p) => [p, p === 3000 ? { status: 200, note: "HTTP 200" } : { status: null, note: "not HTTP" }])))
  const now = Date.now()
  const out = await Promise.all(
    ports.map(async (port) => {
      const hit = cache.get(port)
      if (hit && now - hit.at < 10_000) return [port, hit.p] as const
      const p = await head(port)
      cache.set(port, { at: Date.now(), p })
      return [port, p] as const
    }),
  )
  return Response.json(Object.fromEntries(out))
}
