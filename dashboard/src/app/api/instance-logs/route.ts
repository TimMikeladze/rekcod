import { FIXTURE, spawnRekcod } from "@/lib/rekcod"
import { fakeLogs } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { cappedStream, fakeStream } from "@/lib/server/stream"
import { instanceLogsArgv } from "@/lib/server/validate"

/** `rekcod app logs APP --instance ID` for an instance the state lists. */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const a = instanceLogsArgv(g.s, g.q.get("app"), g.q.get("id"))
  if ("error" in a) return bad(a.error)
  if (FIXTURE) return fakeStream(req, fakeLogs(false, 80))
  return cappedStream(req, () => spawnRekcod(a.argv))
}
