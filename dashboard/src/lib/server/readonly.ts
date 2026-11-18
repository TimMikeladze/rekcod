import "server-only"
import { refuse } from "@/lib/guard"
import type { State } from "@/lib/types"
import { cached, get } from "./hub"

/**
 * Guard plus the state names are checked against: the cache, or a read if there's none yet.
 * `engine` is the one that state was read from; commands built from it run there.
 */
export async function guarded(req: Request): Promise<{ no: Response } | { s: State; q: URLSearchParams; engine: string }> {
  const no = refuse(req, false)
  if (no) return { no }
  try {
    const s = cached() ?? (await get())
    return { s, q: new URL(req.url).searchParams, engine: s.engine ?? "local" }
  } catch (e) {
    return { no: Response.json({ error: String((e as Error).message) }, { status: 502 }) }
  }
}

export const bad = (error: string) => Response.json({ error }, { status: 400 })
