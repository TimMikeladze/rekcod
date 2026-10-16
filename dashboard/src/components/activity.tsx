"use client"

import { Suspense, useEffect, useMemo, useState } from "react"
import { usePathname, useRouter, useSearchParams } from "next/navigation"
import { History, RotateCcw } from "lucide-react"
import { useAction } from "@/components/actions"
import { Empty, Mono, State } from "@/components/bits"
import { PageTitle } from "@/components/shell/page-title"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { useDash } from "@/hooks/dash-context"
import { allActions } from "@/lib/allow"
import { ago } from "@/lib/format"
import type { ActivityEntry } from "@/lib/types"

/** Failed runs nobody has opened yet. */
export function unseenFailures(list: ActivityEntry[]): ActivityEntry[] {
  return list.filter((e) => e.code !== 0 && !e.seen)
}

export function ActivityButton({ onOpen }: { onOpen: () => void }) {
  const { activity } = useDash()
  const bad = unseenFailures(activity).length
  return (
    <Button
      size="sm"
      variant="ghost"
      onClick={onOpen}
      className={`text-xs ${bad ? "text-alert-text" : "text-muted-foreground"}`}
      aria-label={bad ? `Activity: ${bad} failed action${bad > 1 ? "s" : ""} not looked at` : "Activity"}
      title="Actions this page ran"
    >
      <History />
      <span className="hidden sm:inline">Activity</span>
      {bad > 0 && <span className="rounded-full bg-alert px-1.5 font-mono text-[10px] text-white tabular-nums">{bad}</span>}
    </Button>
  )
}

/** Runs that pass the Activity filters: failed only, one subject, text in the label, subject, argv or output. */
export function filterActivity(list: ActivityEntry[], f: { failed: boolean; subject: string; q: string }): ActivityEntry[] {
  const q = f.q.trim().toLowerCase()
  return list.filter(
    (e) =>
      (!f.failed || e.code !== 0) &&
      (!f.subject || e.subject === f.subject) &&
      (!q || `${e.label} ${e.subject} rekcod ${e.argv.join(" ")} ${e.stdout} ${e.stderr}`.toLowerCase().includes(q)),
  )
}

/** Every action this dashboard ran (kept on the server until it restarts), newest first, with filters in the URL. */
export function ActivityPage() {
  return (
    <Suspense>
      <ActivityBody />
    </Suspense>
  )
}

function ActivityBody() {
  const { activity, markSeen, state } = useDash()
  const { trigger } = useAction()
  const params = useSearchParams()
  const router = useRouter()
  const pathname = usePathname()
  const runParam = Number(params.get("run")) || null
  const [expanded, setExpanded] = useState<number | null>(runParam)
  const f = { failed: params.get("failed") === "1", subject: params.get("subject") ?? "", q: params.get("q") ?? "" }
  const set = (k: string, v: string) => {
    const p = new URLSearchParams(params.toString())
    if (v) p.set(k, v)
    else p.delete(k)
    p.delete("run")
    const qs = p.toString()
    router.replace(qs ? `${pathname}?${qs}` : pathname, { scroll: false })
  }
  // Every argv the state offers now; walking the state once, not once per run.
  const offered = useMemo(() => new Map(state ? allActions(state).filter((a) => a.argv).map((a) => [JSON.stringify(a.argv), a] as const) : []), [state])
  const subjects = useMemo(() => [...new Set(activity.map((e) => e.subject).filter(Boolean))].sort(), [activity])
  const shown = filterActivity(activity, f)
  const filtered = f.failed || !!f.subject || !!f.q
  const toggle = (e: ActivityEntry) => {
    setExpanded(expanded === e.id ? null : e.id)
    if (!e.seen) markSeen([e.id])
  }
  // `?run=ID` (a toast's View): open it, mark it seen and bring it into view once it's listed.
  const linked = runParam != null && activity.some((e) => e.id === runParam)
  useEffect(() => {
    if (!linked || runParam == null) return
    document.getElementById(`run-${runParam}`)?.scrollIntoView({ block: "center" })
    const e = activity.find((x) => x.id === runParam)
    if (e && !e.seen) markSeen([e.id])
    // Once per linked run, not on every new entry.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [linked, runParam])
  return (
    <div className="space-y-6">
      <PageTitle title="Activity">What this dashboard ran since its server started. Failed runs stay red until you open them.</PageTitle>
      {activity.length > 0 && (
        <div role="group" aria-label="Filter runs" className="flex flex-wrap items-center gap-2">
          <Button size="sm" variant={f.failed ? "secondary" : "outline"} aria-pressed={f.failed} onClick={() => set("failed", f.failed ? "" : "1")}>
            Failed only
          </Button>
          <select
            value={f.subject}
            onChange={(e) => set("subject", e.target.value)}
            aria-label="Subject"
            className="h-8 rounded-md border bg-background px-2 text-[13px]"
          >
            <option value="">Every subject</option>
            {subjects.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
          <Input value={f.q} onChange={(e) => set("q", e.target.value)} placeholder="Search runs and output" aria-label="Search runs" className="h-8 w-full sm:w-64" />
          <span className="cap ml-auto tabular-nums">
            {filtered ? `${shown.length} of ${activity.length}` : `${activity.length} run${activity.length === 1 ? "" : "s"}`}
          </span>
        </div>
      )}
      {!activity.length ? (
        <Empty>Nothing run yet. Buttons, the row menus and Cmd+K all land here.</Empty>
      ) : !shown.length ? (
        <Empty>No run matches these filters.</Empty>
      ) : (
        <ul className="divide-y divide-hairline overflow-hidden rounded-lg border bg-card">
          {shown.map((e) => {
            const failed = e.code !== 0
            const open = expanded === e.id
            // Only when the same argv is still offered: the server would refuse anything else.
            const again = offered.get(JSON.stringify(e.argv)) ?? null
            return (
              <li key={e.id} id={`run-${e.id}`} className={failed && !e.seen ? "bg-alert-bg" : ""}>
                <div className="flex items-start">
                  <button
                    onClick={() => toggle(e)}
                    aria-expanded={open}
                    className="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-3 gap-y-0.5 px-4 py-2 text-left hover:bg-subtle focus-visible:bg-subtle focus-visible:outline-none"
                  >
                    <State tone={failed ? "bad" : "ok"}>{failed ? `exit ${e.code}` : "ok"}</State>
                    <span className="text-sm font-medium">
                      {e.label} <span className="text-muted-foreground">· {e.subject}</span>
                    </span>
                    <span className="ml-auto text-xs text-muted-foreground tabular-nums">
                      {e.ms < 1000 ? `${e.ms} ms` : `${(e.ms / 1000).toFixed(1)} s`} · {ago(e.at / 1000)}
                    </span>
                    <Mono className="w-full truncate text-muted-foreground">rekcod {e.argv.join(" ")}</Mono>
                  </button>
                  {again && (
                    <Button
                      size="xs"
                      variant="ghost"
                      className="mt-2 mr-2"
                      onClick={() => trigger(again, e.subject)}
                      aria-label={`Run again: rekcod ${e.argv.join(" ")}`}
                      title={again.confirm ?? `rekcod ${e.argv.join(" ")}`}
                    >
                      <RotateCcw />
                      <span className="max-sm:sr-only">Run again</span>
                    </Button>
                  )}
                </div>
                {open && (
                  <pre className="mx-4 mb-3 max-h-96 overflow-auto rounded-lg border bg-subtle p-3 font-mono text-xs whitespace-pre-wrap">
                    {[e.stdout.trim(), e.stderr.trim()].filter(Boolean).join("\n\n") || (e.terminal ? "opened in Terminal" : "no output")}
                  </pre>
                )}
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
