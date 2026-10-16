import { readFile, rm } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { FIXTURE, run } from "@/lib/rekcod"
import { fakePng } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { screenshotArgv } from "@/lib/server/validate"

/** A running macOS sandbox's screen as PNG (`rekcod sandbox screenshot ID FILE`). */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const file = path.join(os.tmpdir(), `rekcod-dash-${process.pid}-${Date.now()}.png`)
  const a = screenshotArgv(g.s, g.q.get("id"), file)
  if ("error" in a) return bad(a.error)
  const headers = { "content-type": "image/png", "cache-control": "no-store" }
  if (FIXTURE) return new Response(new Uint8Array(fakePng()), { headers })
  const r = await run(a.argv, { timeout: 30_000 })
  if (r.code !== 0) return Response.json({ error: r.stderr.trim() || `exit ${r.code}` }, { status: 502 })
  try {
    return new Response(new Uint8Array(await readFile(file)), { headers })
  } finally {
    rm(file, { force: true }).catch(() => {})
  }
}
