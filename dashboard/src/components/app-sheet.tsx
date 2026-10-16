"use client"

import { useEffect, useState } from "react"
import Link from "next/link"
import { ChevronRight } from "lucide-react"
import { Actions } from "@/components/actions"
import { Empty, Mono, releaseTone, State } from "@/components/bits"
import { CopyCommand } from "@/components/copy-command"
import { LatencyChart, TrafficChart } from "@/components/fleet-charts"
import { NotFound, Output, useTextStream } from "@/components/job-sheet"
import { PinButton } from "@/components/shell/pins"
import { DetailHead } from "@/components/shell/page-title"
import { Head, NavRow } from "@/components/table"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Table, TableBody, TableCell } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, duration } from "@/lib/format"
import type { Release, TrafficMinute } from "@/lib/types"

/** The commit's page on GitHub, GitLab or Bitbucket; null for other hosts. */
export function commitUrl(git: string, sha: string): string | null {
  const m = /^(?:https?:\/\/|git@|ssh:\/\/git@)(github\.com|gitlab\.com|bitbucket\.org)[/:](.+?)(?:\.git)?\/?$/.exec(git.trim())
  if (!m) return null
  const [, host, repo] = m
  if (host === "github.com") return `https://github.com/${repo}/commit/${sha}`
  if (host === "gitlab.com") return `https://gitlab.com/${repo}/-/commit/${sha}`
  return `https://bitbucket.org/${repo}/commits/${sha}`
}

/** The release `app rollback` picks: the newest live or old one below the live release. */
export function rollbackTarget(releases: Release[], live: number | null): number | null {
  if (live == null) return null
  const ok = releases.filter((r) => r.n < live && (r.state === "live" || r.state === "old"))
  return ok.length ? Math.max(...ok.map((r) => r.n)) : null
}

/**
 * Requests in the last full minute; null when the controller doesn't report traffic. The
 * edges report a minute once it's over, so the newest row is at most a minute or two old.
 */
export function requestsPerMinute(traffic: TrafficMinute[] | undefined, now: number): number | null {
  if (!traffic) return null
  const last = traffic.at(-1)
  return last && Math.floor(now / 60) - last.minute <= 2 ? last.requests : 0
}

function ReleaseRow({ app, r, gitUrl, live, target, now }: { app: string; r: Release; gitUrl: string; live: number | null; target: number | null; now: number }) {
  const [open, setOpen] = useState(false)
  const [log, setLog] = useState<string | null>(null)
  useEffect(() => {
    if (!open || log != null) return
    const ctl = new AbortController()
    fetch(`/api/release?app=${encodeURIComponent(app)}&n=${r.n}`, { signal: ctl.signal, cache: "no-store" })
      .then(async (res) => {
        const body = await res.json().catch(() => ({ error: res.statusText }))
        setLog(body.error ? `(${body.error})` : body.log || "(the controller kept no deploy log for this release)")
      })
      .catch((e) => !ctl.signal.aborted && setLog(`(${String(e)})`))
    return () => ctl.abort()
  }, [open, log, app, r.n])
  const link = commitUrl(gitUrl, r.commit)
  const took = r.finished ? duration(Math.max(1, r.finished - r.created)) : r.state === "building" ? "building…" : null
  return (
    <li className="relative">
      <span className="absolute top-1.5 -left-[1.3rem]">
        <State tone={releaseTone(r.state)}>
          <span className="sr-only">{r.state}</span>
        </State>
      </span>
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <button
          className="flex items-center gap-1 font-mono text-xs font-medium hover:underline"
          aria-expanded={open}
          aria-label={`Release ${r.n} deploy log`}
          onClick={() => setOpen(!open)}
        >
          <ChevronRight className={`size-3 transition-transform ${open ? "rotate-90" : ""}`} />#{r.n}
        </button>
        {link ? (
          <a href={link} target="_blank" rel="noreferrer" className="font-mono text-xs text-muted-foreground underline-offset-2 hover:underline">
            {r.commit.slice(0, 10)}
          </a>
        ) : (
          <Mono className="text-muted-foreground">{r.commit.slice(0, 10)}</Mono>
        )}
        {r.n === live ? <Badge variant="secondary">live</Badge> : <span className="text-xs">{r.state}</span>}
        {r.n === target && (
          <Badge variant="outline" title="`rekcod app rollback` deploys this one">
            rollback target
          </Badge>
        )}
        <span className="ml-auto text-xs text-muted-foreground">{ago(r.created, now)}</span>
      </div>
      <div className="mt-0.5 pl-4 text-xs text-muted-foreground">
        {[r.who && `by ${r.who}`, took && `took ${took}`].filter(Boolean).join(" · ") || " "}
      </div>
      {open && <Output text={log ?? ""} empty="loading…" className="mt-1.5 max-h-60" />}
    </li>
  )
}

function InstanceLogs({ app, id }: { app: string; id: string }) {
  const { text, done } = useTextStream(`/api/instance-logs?app=${encodeURIComponent(app)}&id=${encodeURIComponent(id)}`)
  return <Output text={text} empty={done ? "no output" : "loading…"} className="max-h-72" />
}

/** `/apps/[name]`: traffic, settings, instances (with logs), releases (with deploy logs). */
export function AppDetail({ name }: { name: string }) {
  const { state } = useDash()
  const app = state?.apps.find((a) => a.name === name)
  const now = state?.generated ?? 0
  const d = app?.detail
  const [inst, setInst] = useState<string | null>(null)
  const [scale, setScale] = useState("")
  const [env, setEnv] = useState("")
  if (!app) return <NotFound what={`App ${name}`} />
  const traffic = d?.traffic ?? []
  const rpm = requestsPerMinute(d?.traffic, now)
  const target = d ? rollbackTarget(d.releases, app.live_release) : null
  return (
          <div>
            <DetailHead
              title={app.name}
              sub={
                <a href={app.url} target="_blank" rel="noreferrer" className="hover:underline">
                  {app.url}
                </a>
              }
            >
              <Actions actions={app.actions} subject={app.name} inline={4} toolbar />
              <PinButton pin={{ kind: "apps", name: app.name }} />
            </DetailHead>
            {!d ? (
              <Empty>The controller didn&apos;t return details for this app.</Empty>
            ) : (
              <div className="mt-6 space-y-6 text-sm">
                <section>
                  <h2 className="h2 mb-3 flex items-baseline">
                    Traffic, last hour
                    {rpm != null && <span className="ml-auto font-mono normal-case">{rpm} req/min now</span>}
                  </h2>
                  {d.traffic == null ? (
                    <p className="text-xs text-muted-foreground">This controller doesn&apos;t record traffic (it predates it).</p>
                  ) : !traffic.length ? (
                    <p className="text-xs text-muted-foreground">No requests through the edge in the last hour.</p>
                  ) : traffic.length < 2 ? (
                    <p className="text-xs text-muted-foreground">One minute recorded so far; the charts start with the second.</p>
                  ) : (
                    <div className="space-y-3">
                      <TrafficChart rows={traffic} />
                      <LatencyChart rows={traffic} />
                      <p className="text-xs text-muted-foreground">
                        {traffic.reduce((a, t) => a + t.requests, 0)} requests, {traffic.reduce((a, t) => a + t.errors, 0)} 5xx in the last hour. With
                        several edges, the percentiles are the worst edge&apos;s.
                      </p>
                    </div>
                  )}
                </section>
                <dl className="facts grid-cols-[8rem_1fr]">
                  <dt className="text-muted-foreground">Source</dt>
                  <dd className="font-mono text-xs break-all">
                    {d.git.url}@{d.git.ref}
                  </dd>
                  <dt className="text-muted-foreground">Port</dt>
                  <dd className="font-mono text-xs">{d.port || "from rekcod.toml"}</dd>
                  <dt className="text-muted-foreground">Instances</dt>
                  <dd className="font-mono text-xs">{d.instances_wanted}</dd>
                  <dt className="text-muted-foreground">Scale to zero</dt>
                  <dd className="font-mono text-xs">{d.idle_stop_secs ? `after ${duration(d.idle_stop_secs)} idle` : "off"}</dd>
                  <dt className="text-muted-foreground">Recycle</dt>
                  <dd className="font-mono text-xs">{d.recycle_secs ? `every ${duration(d.recycle_secs)}` : "off"}</dd>
                  <dt className="text-muted-foreground">Last request</dt>
                  <dd className="text-xs">{ago(d.last_request, now)}</dd>
                  <dt className="text-muted-foreground">Env</dt>
                  <dd className="font-mono text-xs break-all">{Object.keys(d.env ?? {}).join(", ") || "–"}</dd>
                  <dt className="text-muted-foreground">Secrets</dt>
                  <dd className="font-mono text-xs break-all">{d.secrets.join(", ") || "–"}</dd>
                </dl>
                <section>
                  <h2 className="h2 mb-3">Instances</h2>
                  {d.instances.length ? (
                    <>
                      <div className="overflow-x-auto">
                        <Table>
                          <Head cols={["Instance", "Node", ["Up", "hidden sm:table-cell"], "State"]} />
                          <TableBody>
                            {d.instances.map((i) => (
                              <NavRow
                                key={i.sandbox}
                                label={`Instance ${i.sandbox} on ${i.node}, ${i.state}; Enter shows its logs`}
                                onOpen={() => setInst(inst === i.sandbox ? null : i.sandbox)}
                                className={inst === i.sandbox ? "bg-muted/50" : ""}
                              >
                                <TableCell>
                                  <Mono>{i.sandbox}</Mono>
                                  <Mono className="block text-muted-foreground">release #{i.release}</Mono>
                                </TableCell>
                                <TableCell>
                                  <Link
                                    href={`/nodes/${encodeURIComponent(i.node)}`}
                                    className="font-mono text-xs underline-offset-2 hover:underline"
                                    onClick={(e) => e.stopPropagation()}
                                  >
                                    {i.node}
                                  </Link>
                                </TableCell>
                                <TableCell className="hidden text-xs text-muted-foreground sm:table-cell">
                                  {i.created ? duration(Math.max(1, now - i.created)) : "–"}
                                </TableCell>
                                <TableCell>
                                  <State tone={releaseTone(i.state)}>{i.state}</State>
                                </TableCell>
                              </NavRow>
                            ))}
                          </TableBody>
                        </Table>
                      </div>
                      {inst ? (
                        <div className="mt-2">
                          <div className="mb-1 text-xs text-muted-foreground">
                            Logs of <Mono>{inst}</Mono>
                          </div>
                          <InstanceLogs key={inst} app={app.name} id={inst} />
                        </div>
                      ) : (
                        <p className="mt-1 text-xs text-muted-foreground">Click an instance for its logs.</p>
                      )}
                    </>
                  ) : (
                    <p className="text-xs text-muted-foreground">No instances{app.sleeping ? " (scaled to zero)" : ""}.</p>
                  )}
                </section>
                <section>
                  <h2 className="h2 mb-3">Releases</h2>
                  <ol className="relative space-y-3 border-l pl-4">
                    {d.releases.map((r) => (
                      <ReleaseRow key={r.n} app={app.name} r={r} gitUrl={d.git.url} live={app.live_release} target={target} now={now} />
                    ))}
                  </ol>
                </section>
                <section className="space-y-2">
                  <h2 className="h2">Deploy, scale, env</h2>
                  <p className="text-xs text-muted-foreground">These take arguments, so they run from the CLI. Copy the line, then run it in the app&apos;s repo.</p>
                  <CopyCommand args={["app", "deploy", "--name", app.name, "--git", d.git.url, "--ref", d.git.ref]} label="deploy" />
                  <div className="flex items-center gap-2">
                    <label htmlFor="scale-n" className="w-20 shrink-0 text-xs text-muted-foreground">
                      Instances
                    </label>
                    <Input id="scale-n" inputMode="numeric" value={scale} onChange={(e) => setScale(e.target.value.replace(/\D/g, ""))} placeholder={String(d.instances_wanted)} className="h-8 w-20 text-xs" />
                  </div>
                  <CopyCommand args={["app", "scale", app.name, scale || String(d.instances_wanted)]} label="scale" />
                  <div className="flex items-center gap-2">
                    <label htmlFor="env-kv" className="w-20 shrink-0 text-xs text-muted-foreground">
                      KEY=VALUE
                    </label>
                    <Input id="env-kv" value={env} onChange={(e) => setEnv(e.target.value)} placeholder="LOG_LEVEL=debug" className="h-8 text-xs" />
                  </div>
                  <CopyCommand args={["app", "env", "set", app.name, ...(env.trim() ? env.trim().split(/\s+/) : ["KEY=VALUE"])]} label="env set" />
                </section>
              </div>
            )}
          </div>
  )
}
