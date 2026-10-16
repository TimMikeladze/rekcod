import { FIXTURE, spawnRekcod } from "@/lib/rekcod"
import { fakeLogLine, fakeLogs } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { fakeStream, textStream } from "@/lib/server/stream"
import { logsArgv } from "@/lib/server/validate"

// Container logs with the sheet's options: `follow`, `timestamps`, `since` (1m, 10m, 1h, 6h,
// 24h), always `--tail 1000`. Read-only, so not an action (docs/dashboard.md).
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const a = logsArgv(g.s, g.q)
  if ("error" in a) return bad(a.error)
  if (FIXTURE) {
    const ts = a.argv.includes("--timestamps")
    return fakeStream(req, fakeLogs(ts), a.argv.includes("--follow") ? () => fakeLogLine(0, ts) : undefined)
  }
  return textStream(req, spawnRekcod(a.argv))
}
