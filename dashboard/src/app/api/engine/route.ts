import { refuse } from "@/lib/guard"
import { readState } from "@/lib/rekcod"
import { engine } from "@/lib/server/engine"
import { switchEngine } from "@/lib/server/hub"

// The engine the dashboard shows: `local` or a docker host (`rekcod host ls`).
export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  return Response.json({ engine: engine() })
}

export async function POST(req: Request) {
  const no = refuse(req, true)
  if (no) return no
  const { engine: name } = (await req.json().catch(() => ({}))) as { engine?: unknown }
  if (typeof name !== "string") return Response.json({ error: "engine is a host name or local" }, { status: 400 })
  if (name !== "local") {
    const hosts = (await readState(true)).hosts.all.map((h) => h.name)
    if (!hosts.includes(name)) return Response.json({ error: `no host ${name}; rekcod host ls` }, { status: 400 })
  }
  if (name !== engine()) switchEngine(name).catch(() => {})
  return Response.json({ engine: name })
}
