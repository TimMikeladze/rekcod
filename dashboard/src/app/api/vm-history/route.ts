import { refuse } from "@/lib/guard"
import { fsHistory } from "@/lib/server/hub"

/** virtio-fs request counters from each state the server read, the last hour. */
export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  return Response.json({ fs: fsHistory() })
}
