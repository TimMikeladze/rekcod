import { refuse } from "@/lib/guard"
import { stats } from "@/lib/server/sampler"

// Read-only: the latest `docker stats` sample per container and the server's history.
export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  const w = Number(new URL(req.url).searchParams.get("window"))
  return Response.json(await stats([300, 900, 3600].includes(w) ? w : 300))
}
