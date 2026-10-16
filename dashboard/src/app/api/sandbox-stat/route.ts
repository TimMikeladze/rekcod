import { FIXTURE, run } from "@/lib/rekcod"
import { FAKE_DIFF_STAT } from "@/lib/server/fixtures"
import { bad, guarded } from "@/lib/server/readonly"
import { sandboxStatArgv } from "@/lib/server/validate"

/** `rekcod sandbox diff --stat ID`: the files a copy sandbox changed, as text. */
export async function GET(req: Request) {
  const g = await guarded(req)
  if ("no" in g) return g.no
  const a = sandboxStatArgv(g.s, g.q.get("id"))
  if ("error" in a) return bad(a.error)
  const text = FIXTURE ? FAKE_DIFF_STAT : await run(a.argv, { timeout: 30_000 }).then((r) => (r.code ? r.stderr || `exit ${r.code}` : r.stdout))
  return new Response(text, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" } })
}
