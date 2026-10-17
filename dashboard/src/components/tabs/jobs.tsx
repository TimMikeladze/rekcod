"use client"

import Link from "next/link"
import { useRouter } from "next/navigation"
import { Actions } from "@/components/actions"
import { Empty, jobTone, Mono, State } from "@/components/bits"
import { JobsTimeline } from "@/components/fleet-charts"
import { GetStarted, NoRemote } from "@/components/get-started"
import { cmdLine, Head, match, NavRow, SectionTitle } from "@/components/table"
import { RemoteError, remoteError } from "@/components/tabs/fleet"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { useSort } from "@/hooks/use-sort"
import { ago, duration } from "@/lib/format"
import type { Job } from "@/lib/types"

export function Jobs({ q = "" }: { q?: string }) {
  const { state } = useDash()
  const router = useRouter()
  const open = (id: string) => router.push(`/jobs/${encodeURIComponent(id)}`)
  const now = state?.generated ?? 0
  const took = (j: Job) => (j.created == null ? null : j.finished ? j.finished - j.created : j.state === "running" ? now - j.created : null)
  const sort = useSort<Job>({ name: (j) => j.name ?? j.id, started: (j) => j.created, took, result: (j) => j.state })
  if (!state) return null
  if (!state.remote) return <NoRemote what="Jobs" />
  const err = remoteError(state, "jobs")
  const list = state.jobs
  const schedules = state.schedules ?? []
  const rows = sort.apply(list.filter((j) => match(q, j.id, j.name, j.state, j.node, j.schedule, cmdLine(j.cmd))))
  return (
    <div className="space-y-6">
      {!err && list.length > 0 && (
        <section aria-labelledby="timeline-h">
          <SectionTitle>
            <span id="timeline-h">Last 24 hours</span>
          </SectionTitle>
          <div className="rounded-lg border bg-card p-4">
            <JobsTimeline jobs={list} now={now} />
          </div>
        </section>
      )}
      {err ? (
        <RemoteError error={err} what="jobs" />
      ) : !list.length ? (
        <GetStarted
          title="No jobs yet"
          commands={[
            ["job", "run", "--", "python", "etl.py"],
            ["job", "run", "--name", "nightly", "--every", "24h", "--", "python", "etl.py"],
          ]}
        >
          A job runs one command in a fresh sandbox on a node and keeps its output. --every makes it a schedule.
        </GetStarted>
      ) : !rows.length ? (
        <Empty>Nothing matches “{q}”.</Empty>
      ) : (
        <div className="overflow-x-auto">
          <Table>
            <Head
              sort={sort}
              cols={[["Job", "", "name"], ["Command", "hidden md:table-cell"], ["Node", "hidden sm:table-cell"], ["Started", "hidden sm:table-cell", "started"], ["Took", "hidden sm:table-cell", "took"], ["Result", "", "result"], ""]}
            />
            <TableBody>
              {rows.map((j) => (
                <NavRow key={j.id} label={`Job ${j.name ?? j.id}, ${j.state}`} watch={j.state} onOpen={() => open(j.id)}>
                  <TableCell>
                    <div className="font-medium">{j.name ?? <Mono>{j.id}</Mono>}</div>
                    {j.name && <Mono className="text-muted-foreground">{j.id}</Mono>}
                    {j.schedule && (
                      <Badge variant="outline" className="ml-1">
                        {j.schedule}
                      </Badge>
                    )}
                    <Mono className="block max-w-40 truncate text-muted-foreground sm:max-w-56 md:hidden">{cmdLine(j.cmd)}</Mono>
                  </TableCell>
                  <TableCell className="hidden max-w-72 truncate md:table-cell">
                    <Mono className="text-muted-foreground">{cmdLine(j.cmd) || "–"}</Mono>
                  </TableCell>
                  <TableCell className="hidden sm:table-cell">
                    <Mono>{j.node ?? "–"}</Mono>
                  </TableCell>
                  <TableCell className="hidden text-xs text-muted-foreground sm:table-cell">{j.created != null ? ago(j.created, now) : "?"}</TableCell>
                  <TableCell className="hidden sm:table-cell">
                    <Mono>{took(j) != null ? duration(Math.max(1, took(j)!)) : "–"}</Mono>
                  </TableCell>
                  <TableCell>
                    <State tone={jobTone(j.state, j.exit)} pulse={j.state === "running"}>
                      {j.state}
                      {j.exit != null && j.state !== "running" ? ` · exit ${j.exit}` : ""}
                    </State>
                  </TableCell>
                  <TableCell>
                    <Actions actions={j.actions} subject={j.name ?? j.id} inline={1} />
                  </TableCell>
                </NavRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      {schedules.length > 0 && (
        <section>
          <SectionTitle>Schedules</SectionTitle>
          <div className="overflow-x-auto">
            <Table>
              <Head cols={["Name", "Every", ["Owner", "hidden sm:table-cell"], "Last run", "Next run", ["Last job", "hidden sm:table-cell"]]} />
              <TableBody>
                {schedules.map((s) => {
                  const last = list.find((j) => j.id === s.last_job)
                  const running = last?.state === "running"
                  return (
                    <TableRow key={s.name}>
                      <TableCell className="font-medium">{s.name}</TableCell>
                      <TableCell>
                        <Mono>{duration(s.every_secs)}</Mono>
                      </TableCell>
                      <TableCell className="hidden text-muted-foreground sm:table-cell">{s.owner}</TableCell>
                      <TableCell className="text-xs text-muted-foreground">{ago(s.last_run, now)}</TableCell>
                      <TableCell className="text-xs">
                        {running ? (
                          <span className="text-muted-foreground">after the running one finishes</span>
                        ) : s.next_run == null ? (
                          "–"
                        ) : s.next_run <= now + 5 ? (
                          "now"
                        ) : (
                          `in ${duration(s.next_run - now)}`
                        )}
                      </TableCell>
                      <TableCell className="hidden sm:table-cell">
                        {s.last_job ? (
                          last ? (
                            <Link href={`/jobs/${encodeURIComponent(last.id)}`} className="font-mono text-xs text-muted-foreground hover:underline">
                              {s.last_job}
                            </Link>
                          ) : (
                            <Mono className="text-muted-foreground">{s.last_job}</Mono>
                          )
                        ) : (
                          <Mono className="text-muted-foreground">–</Mono>
                        )}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        </section>
      )}
    </div>
  )
}
