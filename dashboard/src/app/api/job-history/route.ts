import { FIXTURE, run } from "@/lib/rekcod"
import { fakeJobHistory } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { jobHistoryArgv } from "@/lib/server/validate"

/** `rekcod job ls --name N`: the last 50 runs of a named or scheduled job, `{jobs, schedules}`. */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const a = jobHistoryArgv(g.s, g.q.get("name"))
  if ("error" in a) return bad(a.error)
  if (FIXTURE) return Response.json(fakeJobHistory(a.argv[3], g.s))
  const r = await run(a.argv, { timeout: 20_000 })
  if (r.code !== 0) return Response.json({ error: r.stderr.trim() || `exit ${r.code}` }, { status: 502 })
  return new Response(r.stdout, { headers: { "content-type": "application/json" } })
}
