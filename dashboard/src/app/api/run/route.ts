import { refuse } from "@/lib/guard"
import { fill, type Values } from "@/lib/inputs"
import { FIXTURE, inTerminal, offered, run, STREAMED } from "@/lib/rekcod"
import { record } from "@/lib/server/activity"
import { nudge, refreshFull } from "@/lib/server/hub"

/** `{key: [values]}` from the page, strings only. */
function values(v: unknown): Values {
  if (!v || typeof v !== "object") return {}
  return Object.fromEntries(
    Object.entries(v as Record<string, unknown>).map(([k, x]) => [k, (Array.isArray(x) ? x : [x]).filter((s): s is string => typeof s === "string")]),
  )
}

// Runs one of the actions `rekcod state` currently offers; anything else is refused. An action
// with inputs is offered as a template (`{key}` slots); its values are checked and filled in
// here (lib/inputs.ts). Every run goes into the activity log.
export async function POST(req: Request) {
  const no = refuse(req, true)
  if (no) return no
  const body = (await req.json().catch(() => ({}))) as { argv?: unknown; subject?: unknown; values?: unknown }
  const a = await offered(body.argv)
  if (!a) return Response.json({ error: "not an action rekcod offers right now" }, { status: 400 })
  let argv = a.argv!
  if (a.inputs?.length) {
    const f = fill(argv, a.inputs, values(body.values))
    if ("error" in f) return Response.json({ error: f.error }, { status: 400 })
    argv = f.argv
  }
  const t0 = Date.now()
  const base = { at: t0, kind: a.kind, label: a.label, subject: typeof body.subject === "string" ? body.subject.slice(0, 200) : "", argv }
  let r: { code: number; stdout: string; stderr: string; terminal?: boolean }
  if (FIXTURE) r = { code: 0, stdout: `dry run: rekcod ${argv.join(" ")}`, stderr: "" }
  else if (a.interactive && !STREAMED.has(a.kind)) {
    const why = inTerminal({ ...a, argv })
    r = why ? { code: 1, stdout: "", stderr: `not opening Terminal: ${why}` } : { code: 0, stdout: "opened in Terminal", stderr: "", terminal: true }
  } else r = await run(argv, { cwd: a.dir, timeout: 600_000 })
  const entry = record({ ...base, code: r.code, ms: Date.now() - t0, stdout: r.stdout, stderr: r.stderr, terminal: r.terminal })
  nudge()
  refreshFull().catch(() => {})
  return Response.json({ ...r, activity: entry.id })
}
