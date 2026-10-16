"use client"

import { useEffect, useRef, useState } from "react"
import Link from "next/link"
import { Actions } from "@/components/actions"
import { DetailHead } from "@/components/shell/page-title"
import { Empty, jobTone, Mono, State } from "@/components/bits"
import { RunHistoryChart, runResult } from "@/components/fleet-charts"
import { cmdLine, Head } from "@/components/table"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, duration } from "@/lib/format"
import type { Job } from "@/lib/types"

/**
 * Text from `url` as it arrives (a stream, or one response), the last ~400 KB of it. `null`
 * stops. Used for job output and instance logs.
 */
export function useTextStream(url: string | null) {
  const [text, setText] = useState("")
  const [done, setDone] = useState(false)
  useEffect(() => {
    if (!url) return
    const ctl = new AbortController()
    ;(async () => {
      setText("")
      setDone(false)
      try {
        const r = await fetch(url, { signal: ctl.signal, cache: "no-store" })
        if (!r.ok || !r.body) {
          setText(await r.text())
          return
        }
        const reader = r.body.getReader()
        const dec = new TextDecoder()
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          setText((t) => (t + dec.decode(value, { stream: true })).slice(-400_000))
        }
      } catch {
      } finally {
        if (!ctl.signal.aborted) setDone(true)
      }
    })()
    return () => ctl.abort()
  }, [url])
  return { text, done }
}

/** A `<pre>` that stays scrolled to the bottom unless you scroll up. */
export function Output({ text, empty, className }: { text: string; empty: string; className?: string }) {
  const pre = useRef<HTMLPreElement>(null)
  const follow = useRef(true)
  useEffect(() => {
    if (follow.current && pre.current) pre.current.scrollTop = pre.current.scrollHeight
  }, [text])
  return (
    <pre
      ref={pre}
      tabIndex={0}
      onScroll={(e) => {
        const el = e.currentTarget
        follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
      }}
      className={`overflow-auto rounded-lg border bg-subtle p-3 font-mono text-[11px] leading-relaxed whitespace-pre-wrap ${className ?? "max-h-80"}`}
    >
      {text || empty}
    </pre>
  )
}

/** Shown when the id in the URL isn't in the current state. */
export function NotFound({ what }: { what: string }) {
  const { state } = useDash()
  return (
    <div className="space-y-3">
      <DetailHead title={what} />
      <p className="text-sm text-muted-foreground">{state ? `${what} isn't in the current state (gone, or in another namespace).` : "loading…"}</p>
    </div>
  )
}

const took = (j: Job, now: number) =>
  j.finished ? duration(Math.max(1, j.finished - j.created)) : j.state === "running" ? duration(Math.max(1, now - j.created)) : "–"

/** `/jobs/[id]`: one job's command and output (followed while it runs), and the history of a named job. */
export function JobDetail({ id }: { id: string }) {
  const { state } = useDash()
  const job = state?.jobs.find((j) => j.id === id)
  const now = state?.generated ?? 0
  const logs = job?.actions.find((a) => a.kind === "JobLogs")
  const url = logs?.argv ? `/api/stream?argv=${encodeURIComponent(JSON.stringify(logs.argv))}` : null
  const { text, done } = useTextStream(url)
  const named = job?.schedule ?? job?.name ?? null
  const [runs, setRuns] = useState<{ name: string; jobs: Job[] } | null>(null)
  const [runsError, setRunsError] = useState<string | null>(null)

  useEffect(() => {
    if (!named) return
    const ctl = new AbortController()
    fetch(`/api/job-history?name=${encodeURIComponent(named)}`, { signal: ctl.signal, cache: "no-store" })
      .then(async (r) => {
        const body = await r.json().catch(() => ({ error: r.statusText }))
        if (!r.ok || body.error) throw new Error(body.error ?? r.statusText)
        setRuns({ name: named, jobs: body.jobs ?? [] })
        setRunsError(null)
      })
      .catch((e) => !ctl.signal.aborted && setRunsError(String((e as Error).message)))
    return () => ctl.abort()
  }, [named])

  if (!job) return <NotFound what={`Job ${id}`} />
  const history = runs && runs.name === named ? runs.jobs : null
  return (
    <div className="space-y-6 text-sm">
      <DetailHead title={job.name ?? job.id} sub={cmdLine(job.cmd) || job.id}>
        <State tone={jobTone(job.state, job.exit)} pulse={job.state === "running"}>
          {job.state}
          {job.exit != null && job.state !== "running" ? ` · exit ${job.exit}` : ""}
        </State>
        {job.schedule && <Badge variant="outline">schedule {job.schedule}</Badge>}
        <div className="ml-auto">
          <Actions actions={job.actions.filter((a) => a.kind !== "JobLogs")} subject={job.name ?? job.id} inline={2} toolbar />
        </div>
      </DetailHead>
      <dl className="facts grid-cols-[8rem_1fr]">
        <dt className="text-muted-foreground">ID</dt>
        <dd className="font-mono">{job.id}</dd>
        <dt className="text-muted-foreground">Node</dt>
        <dd className="font-mono">{job.node ? <Link href={`/nodes/${encodeURIComponent(job.node)}`} className="hover:underline">{job.node}</Link> : "–"}</dd>
        <dt className="text-muted-foreground">Owner</dt>
        <dd>{job.owner ?? "–"}</dd>
        <dt className="text-muted-foreground">Started</dt>
        <dd>{ago(job.created, now)}</dd>
        <dt className="text-muted-foreground">Took</dt>
        <dd className="font-mono">{took(job, now)}</dd>
      </dl>
      <section>
        <h2 className="h2 mb-3">
          Output {job.state === "running" && !done && <span className="normal-case">(following)</span>}
        </h2>
        <Output text={text} empty={done ? "no output" : "waiting for output…"} className="max-h-[60vh]" />
      </section>
      {named && (
        <section>
          <h2 className="h2 mb-3">Last runs of {named}</h2>
          {runsError ? (
            <p className="text-xs text-alert-text">{runsError}</p>
          ) : !history ? (
            <p className="text-xs text-muted-foreground">loading…</p>
          ) : !history.length ? (
            <Empty>No runs recorded.</Empty>
          ) : (
            <>
              <RunHistoryChart runs={history} />
              <div className="mt-2 max-h-80 overflow-auto">
                <Table>
                  <Head cols={["Run", "Started", "Took", "Result"]} />
                  <TableBody>
                    {history.map((r) => (
                      <TableRow key={r.id}>
                        <TableCell>
                          <Link href={`/jobs/${encodeURIComponent(r.id)}`} className="font-mono text-xs hover:underline">
                            {r.id}
                          </Link>
                        </TableCell>
                        <TableCell className="text-xs text-muted-foreground">{ago(r.created, now)}</TableCell>
                        <TableCell>
                          <Mono>{took(r, now)}</Mono>
                        </TableCell>
                        <TableCell>
                          <State tone={jobTone(r.state, r.exit)}>
                            {r.state === "running" ? "running" : runResult(r)}
                            {r.exit != null && r.state !== "running" ? ` · exit ${r.exit}` : ""}
                          </State>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </>
          )}
        </section>
      )}
    </div>
  )
}
