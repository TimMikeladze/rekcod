import { FIXTURE, run } from "@/lib/rekcod"
import { fakeRelease } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { releaseArgv } from "@/lib/server/validate"

/** `rekcod app release APP N`: one release with its deploy log. */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const a = releaseArgv(g.s, g.q.get("app"), g.q.get("n"))
  if ("error" in a) return bad(a.error)
  if (FIXTURE) return Response.json(fakeRelease(a.argv[2], Number(a.argv[3]), g.s))
  const r = await run(a.argv, { timeout: 20_000 })
  if (r.code !== 0) return Response.json({ error: r.stderr.trim() || `exit ${r.code}` }, { status: 502 })
  return new Response(r.stdout, { headers: { "content-type": "application/json" } })
}
