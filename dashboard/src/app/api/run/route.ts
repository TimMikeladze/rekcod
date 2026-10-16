import { refuse } from "@/lib/guard"
import { FIXTURE, inTerminal, offered, run, STREAMED } from "@/lib/rekcod"
import { record } from "@/lib/server/activity"
import { nudge, refreshFull } from "@/lib/server/hub"

// Runs one of the actions `rekcod state` currently offers; anything else is refused. Every run
// goes into the activity log.
export async function POST(req: Request) {
  const no = refuse(req, true)
  if (no) return no
  const { argv, subject } = (await req.json().catch(() => ({}))) as { argv?: unknown; subject?: unknown }
  const a = await offered(argv)
  if (!a) return Response.json({ error: "not an action rekcod offers right now" }, { status: 400 })
  const t0 = Date.now()
  const base = { at: t0, kind: a.kind, label: a.label, subject: typeof subject === "string" ? subject.slice(0, 200) : "", argv: a.argv! }
  let r: { code: number; stdout: string; stderr: string; terminal?: boolean }
  if (FIXTURE) r = { code: 0, stdout: `dry run: rekcod ${a.argv!.join(" ")}`, stderr: "" }
  else if (a.interactive && !STREAMED.has(a.kind)) {
    inTerminal(a)
    r = { code: 0, stdout: "opened in Terminal", stderr: "", terminal: true }
  } else r = await run(a.argv!, { cwd: a.dir, timeout: 600_000 })
  const entry = record({ ...base, code: r.code, ms: Date.now() - t0, stdout: r.stdout, stderr: r.stderr, terminal: r.terminal })
  nudge()
  refreshFull().catch(() => {})
  return Response.json({ ...r, activity: entry.id })
}
