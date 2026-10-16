import { refuse } from "@/lib/guard"
import { get } from "@/lib/server/hub"

export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  try {
    return Response.json(await get())
  } catch (e) {
    return Response.json({ error: String((e as Error).message) }, { status: 502 })
  }
}
