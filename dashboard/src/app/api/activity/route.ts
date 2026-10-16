import { refuse } from "@/lib/guard"
import { list, markSeen } from "@/lib/server/activity"

export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  return Response.json(list())
}

/** `{seen: [ids]}`: the page opened these entries. */
export async function POST(req: Request) {
  const no = refuse(req, true)
  if (no) return no
  const { seen } = (await req.json().catch(() => ({}))) as { seen?: unknown }
  if (!Array.isArray(seen) || !seen.every((n) => Number.isInteger(n))) return Response.json({ error: "seen: [ids]" }, { status: 400 })
  markSeen(seen as number[])
  return Response.json({ ok: true })
}
