import { FIXTURE, run, watcherEnv } from "@/lib/rekcod"
import { onEngine } from "@/lib/server/engine"
import { fakeInspect } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { inspectArgv } from "@/lib/server/validate"

/** `rekcod inspect NAME` for a container in the state: the first (only) element. */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const a = inspectArgv(g.s, g.q.get("name"))
  if ("error" in a) return bad(a.error)
  if (FIXTURE) return Response.json(fakeInspect(g.s.containers.find((c) => c.name === a.argv[1])!))
  // Docker passthrough boots a stopped VM; an open page never should.
  const r = await run(onEngine(a.argv, g.engine), { timeout: 20_000, env: watcherEnv() })
  if (r.code !== 0) return Response.json({ error: r.stderr.trim() || `exit ${r.code}` }, { status: 502 })
  try {
    return Response.json(JSON.parse(r.stdout)[0] ?? {})
  } catch {
    return Response.json({ error: "inspect didn't print JSON" }, { status: 502 })
  }
}
