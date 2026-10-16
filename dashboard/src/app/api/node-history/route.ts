import { bad, guarded } from "@/lib/server/readonly"
import { nodeHistory } from "@/lib/server/nodes"

/** A node's load, memory and disk from each full state the server read, the last hour. Runs nothing. */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const id = g.q.get("id")
  if (!id || !g.s.nodes?.some((n) => n.id === id)) return bad("no such node")
  return Response.json({ id, samples: nodeHistory(id) })
}
