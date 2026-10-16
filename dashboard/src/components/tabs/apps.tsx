"use client"

import { useRouter } from "next/navigation"
import { Actions } from "@/components/actions"
import { Empty, Mono, State } from "@/components/bits"
import { TrafficSpark } from "@/components/fleet-charts"
import { GetStarted, NoRemote } from "@/components/get-started"
import { requestsPerMinute } from "@/components/app-sheet"
import { Head, match, NavRow } from "@/components/table"
import { RemoteError, remoteError } from "@/components/tabs/fleet"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { useSort } from "@/hooks/use-sort"
import { ago } from "@/lib/format"
import type { App, TrafficMinute } from "@/lib/types"

/** Apps on the default remote; a row opens `/apps/[name]`. */
/** 5xx share of the last hour's requests (0-1), or null with no traffic. */
export function errorRate(traffic: TrafficMinute[] | undefined): number | null {
  const req = (traffic ?? []).reduce((n, m) => n + m.requests, 0)
  return req ? (traffic ?? []).reduce((n, m) => n + m.errors, 0) / req : null
}

const pct = (r: number) => (r === 0 ? "0%" : r < 0.001 ? "<0.1%" : `${(r * 100).toFixed(r < 0.1 ? 1 : 0)}%`)

export function Apps({ q = "" }: { q?: string }) {
  const { state } = useDash()
  const router = useRouter()
  const now = state?.generated ?? 0
  const sort = useSort<App>({
    name: (a) => a.name,
    rpm: (a) => requestsPerMinute(a.detail?.traffic, now),
    errors: (a) => errorRate(a.detail?.traffic),
  })
  if (!state) return null
  if (!state.remote) return <NoRemote what="Apps" />
  const err = remoteError(state, "apps")
  if (err) return <RemoteError error={err} what="apps" />
  const list = state.apps
  const rows = sort.apply(list.filter((a) => match(q, a.name, a.url, a.detail?.git.url)))
  if (!list.length)
    return (
      <GetStarted title={`No apps in ${state.namespaces.current || "this namespace"}`} commands={[["app", "deploy"], ["app", "deploy", "--ns", state.namespaces.current || "default"]]}>
        From a repo with a rekcod.toml, deploy builds the pushed HEAD on a node and routes it through the edge.
      </GetStarted>
    )
  if (!rows.length) return <Empty>Nothing matches “{q}”.</Empty>
  return (
    <>
      <div className="overflow-x-auto">
        <Table>
          <Head
            sort={sort}
            cols={[
              ["App", "", "name"],
              "Release",
              ["Instances", "hidden sm:table-cell"],
              ["Traffic, 1h", "hidden w-28 lg:table-cell"],
              ["Req/min", "hidden sm:table-cell text-right", "rpm"],
              ["5xx", "hidden sm:table-cell text-right", "errors"],
              ["Source", "hidden 2xl:table-cell"],
              ["Last request", "hidden md:table-cell"],
              ["Access", "hidden md:table-cell"],
              "",
            ]}
          />
          <TableBody>
            {rows.map((a) => {
              const d = a.detail
              const live = d?.instances.filter((i) => i.state === "live").length ?? 0
              const lost = d?.instances.filter((i) => i.state === "lost").length ?? 0
              const latest = d?.releases[0]
              const failed = latest && latest.state === "failed"
              const rpm = requestsPerMinute(d?.traffic, now)
              const rate = errorRate(d?.traffic)
              const tone = a.sleeping ? "idle" : a.live_release == null || lost ? "warn" : "ok"
              const word = a.sleeping ? "asleep" : a.live_release == null ? "not live" : lost ? `${lost} lost` : "live"
              return (
                <NavRow key={a.name} label={`App ${a.name}, ${word}`} watch={`${word}:${a.live_release}`} onOpen={() => router.push(`/apps/${encodeURIComponent(a.name)}`)}>
                  <TableCell>
                    <State tone={tone}>
                      <span className="font-medium">{a.name}</span>
                      <span className="sr-only">{word}</span>
                    </State>
                    <a
                      href={a.url}
                      target="_blank"
                      rel="noreferrer"
                      onClick={(e) => e.stopPropagation()}
                      className="block max-w-56 truncate pl-3.5 font-mono text-[11px] text-muted-foreground hover:underline"
                    >
                      {a.url.replace(/^https?:\/\//, "")}
                    </a>
                  </TableCell>
                  <TableCell>
                    <Mono>{a.live_release != null ? `#${a.live_release}` : "none"}</Mono>
                    {failed && (
                      <Badge variant="destructive" className="ml-2">
                        #{latest.n} failed
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell className="hidden sm:table-cell">
                    {a.sleeping ? (
                      <span className="text-xs text-muted-foreground">asleep</span>
                    ) : (
                      <Mono>
                        {live}/{d?.instances_wanted ?? "?"}
                        {lost > 0 && <span className="text-alert-text"> · {lost} lost</span>}
                      </Mono>
                    )}
                  </TableCell>
                  <TableCell className="hidden w-28 lg:table-cell">
                    {d?.traffic?.length ? <TrafficSpark rows={d.traffic} app={a.name} /> : <Mono className="text-muted-foreground">–</Mono>}
                  </TableCell>
                  <TableCell className="hidden text-right sm:table-cell">
                    <Mono className={rpm ? "" : "text-muted-foreground"}>{rpm ?? "–"}</Mono>
                  </TableCell>
                  <TableCell className="hidden text-right sm:table-cell">
                    <Mono className={rate == null ? "text-muted-foreground" : rate >= 0.01 ? "text-alert-text" : ""}>{rate == null ? "–" : pct(rate)}</Mono>
                  </TableCell>
                  <TableCell className="hidden max-w-56 truncate 2xl:table-cell">
                    <Mono className="text-muted-foreground">{d ? `${d.git.url.replace(/^https?:\/\/(www\.)?/, "")}@${d.git.ref}` : "–"}</Mono>
                  </TableCell>
                  <TableCell className="hidden text-xs text-muted-foreground md:table-cell">{d ? ago(d.last_request, now) : "–"}</TableCell>
                  <TableCell className="hidden md:table-cell">
                    <Badge variant={a.public ? "secondary" : "outline"}>{a.public ? "public" : "private"}</Badge>
                  </TableCell>
                  <TableCell>
                    <Actions actions={a.actions} subject={a.name} inline={1} />
                  </TableCell>
                </NavRow>
              )
            })}
          </TableBody>
        </Table>
      </div>
    </>
  )
}
