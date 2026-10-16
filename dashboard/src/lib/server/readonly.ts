import "server-only"
import { refuse } from "@/lib/guard"
import type { State } from "@/lib/types"
import { cached, get } from "./hub"

/** Guard plus the state names are checked against: the cache, or a read if there's none yet. */
export async function guarded(req: Request): Promise<{ no: Response } | { s: State; q: URLSearchParams }> {
  const no = refuse(req, false)
  if (no) return { no }
  try {
    return { s: cached() ?? (await get()), q: new URL(req.url).searchParams }
  } catch (e) {
    return { no: Response.json({ error: String((e as Error).message) }, { status: 502 }) }
  }
}

export const bad = (error: string) => Response.json({ error }, { status: 400 })
