"use client"

import { useEffect, useState } from "react"
import Link from "next/link"
import { Actions } from "@/components/actions"
import { jobTone, Meter, Mono, releaseTone, State, type Tone } from "@/components/bits"
import { CopyCommand } from "@/components/copy-command"
import { NodeHistoryChart, type NodeSample } from "@/components/fleet-charts"
import { NotFound } from "@/components/job-sheet"
import { DetailHead } from "@/components/shell/page-title"
import { cmdLine, Head } from "@/components/table"
import { Input } from "@/components/ui/input"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, fmtBytes } from "@/lib/format"
import type { Node } from "@/lib/types"

/** A node's state as one word and a tone: unreachable after a minute without a heartbeat. */
export function nodeHealth(n: Node, now: number): { tone: Tone; text: string; stale: boolean } {
  const stale = now - n.last_seen > 60
  if (stale) return { tone: "bad", text: "unreachable", stale }
  if (n.state === "drain") return { tone: "warn", text: "drained", stale }
  if (n.state === "lost") return { tone: "bad", text: "lost", stale }
  return { tone: "ok", text: n.state, stale }
}

function Gauge({ label, value, text }: { label: string; value: number | null; text: string }) {
  return (
    <div className="rounded-lg border px-3 py-2.5">
      <div className="cap">{label}</div>
      <div className="mt-1 font-mono text-sm tabular-nums">{text}</div>
      {value != null && <Meter value={value} className="mt-1.5" />}
    </div>
  )
}

/** The node's last hour from `/api/node-history`, re-read with each full refresh (15 s). */
function History({ node }: { node: Node }) {
  const [rows, setRows] = useState<{ id: string; samples: NodeSample[] } | null>(null)
  const id = node.id
  useEffect(() => {
    let stop = false
    const load = () =>
      fetch(`/api/node-history?id=${encodeURIComponent(id)}`, { cache: "no-store" })
        .then((r) => r.json())
        .then((b: { id?: string; samples?: NodeSample[] }) => !stop && b.samples && setRows({ id, samples: b.samples }))
        .catch(() => {})
    load()
    const t = setInterval(load, 15_000)
    return () => {
      stop = true
      clearInterval(t)
    }
  }, [id])
  const samples = rows?.id === id ? rows.samples : []
  return (
    <section aria-labelledby="node-history-h">
      <h2 id="node-history-h" className="h2 mb-3">
        Last hour
      </h2>
      <div className="grid gap-3 md:grid-cols-3">
        {(
          [
            ["CPU load", "load", node.cpus],
            ["Memory used", "mem", node.mem_total_mib == null ? undefined : node.mem_total_mib / 1024],
            ["Disk used", "disk", node.disk_total_gib],
          ] as const
        ).map(([label, metric, max]) => (
          <div key={metric} className="rounded-lg border bg-card p-3">
            <div className="cap mb-1">{label}</div>
            <NodeHistoryChart rows={samples} metric={metric} max={max} node={id} />
          </div>
        ))}
      </div>
    </section>
  )
}

/** `/nodes/[id]`: load, memory and disk, what runs on it, drain/undrain, and the pool command. */
export function NodeDetail({ id }: { id: string }) {
  const { state } = useDash()
  const [ns, setNs] = useState("")
  const node = state?.nodes?.find((n) => n.id === id)
  const now = state?.generated ?? 0
  const instances = (state?.apps ?? []).flatMap((a) =>
    (a.detail?.instances ?? []).filter((i) => i.node === id && i.state !== "removed").map((i) => ({ app: a.name, ...i })),
  )
  const jobs = (state?.jobs ?? []).filter((j) => j.node === id && j.state === "running")
  if (!node) return <NotFound what={`Node ${id}`} />
  const total = node.mem_total_mib ?? 0
  const used = total - (node.mem_free_mib ?? 0)
  const h = nodeHealth(node, now)
  return (
          <>
            <DetailHead title={node.id} sub={node.url}>
              <State tone={h.tone}>{h.text}</State>
              <span className="text-xs text-muted-foreground">heartbeat {ago(node.last_seen, now)}</span>
              <div className="ml-auto">
                <Actions actions={node.actions} subject={node.id} inline={2} toolbar />
              </div>
            </DetailHead>
            <div className="mt-6 space-y-6 text-sm">
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
                <Gauge
                  label="CPU"
                  value={node.cpus ? (node.load ?? 0) / node.cpus : null}
                  text={node.cpus ? `load ${node.load?.toFixed(2) ?? "?"} of ${node.cpus}` : "not reported"}
                />
                <Gauge label="Memory" value={total ? used / total : null} text={total ? `${fmtBytes(used * 2 ** 20)} of ${fmtBytes(total * 2 ** 20)}` : "not reported"} />
                <Gauge
                  label="Disk"
                  value={node.disk_total_gib ? 1 - (node.disk_free_gib ?? 0) / node.disk_total_gib : null}
                  text={node.disk_total_gib ? `${node.disk_free_gib} of ${node.disk_total_gib} GiB free` : "not reported"}
                />
              </div>
              <History node={node} />
              <section>
                <h2 className="h2 mb-3">
                  App instances <Mono className="text-muted-foreground">{instances.length}</Mono>
                </h2>
                {instances.length ? (
                  <Table>
                    <Head cols={["App", "Release", "Sandbox", "State"]} />
                    <TableBody>
                      {instances.map((i) => (
                        <TableRow key={i.sandbox}>
                          <TableCell className="font-medium">
                            <Link href={`/apps/${encodeURIComponent(i.app)}`} className="hover:underline">
                              {i.app}
                            </Link>
                          </TableCell>
                          <TableCell>
                            <Mono>#{i.release}</Mono>
                          </TableCell>
                          <TableCell>
                            <Mono className="text-muted-foreground">{i.sandbox}</Mono>
                          </TableCell>
                          <TableCell>
                            <State tone={releaseTone(i.state)}>{i.state}</State>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                ) : (
                  <p className="text-xs text-muted-foreground">None of this namespace&apos;s apps run here.</p>
                )}
              </section>
              <section>
                <h2 className="h2 mb-3">
                  Running jobs <Mono className="text-muted-foreground">{jobs.length}</Mono>
                </h2>
                {jobs.length ? (
                  <Table>
                    <Head cols={["Job", "Command", "Started"]} />
                    <TableBody>
                      {jobs.map((j) => (
                        <TableRow key={j.id}>
                          <TableCell>
                            <State tone={jobTone(j.state, j.exit)} pulse>
                              <Link href={`/jobs/${encodeURIComponent(j.id)}`} className="hover:underline">
                                {j.name ?? <Mono>{j.id}</Mono>}
                              </Link>
                            </State>
                          </TableCell>
                          <TableCell className="max-w-48 truncate">
                            <Mono className="text-muted-foreground">{cmdLine(j.cmd) || "–"}</Mono>
                          </TableCell>
                          <TableCell className="text-xs text-muted-foreground">{ago(j.created, now)}</TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                ) : (
                  <p className="text-xs text-muted-foreground">No running jobs from this namespace.</p>
                )}
              </section>
              <section>
                <h2 className="h2 mb-3">Node pool</h2>
                <p className="mb-2 text-xs text-muted-foreground">
                  {(node.pool ?? []).length
                    ? `Dedicated to ${node.pool!.join(", ")}.`
                    : "Shared: every namespace without its own pool can use it."}{" "}
                  Pools take namespaces as arguments, so they&apos;re set from the CLI.
                </p>
                <label className="mb-1.5 block text-xs text-muted-foreground" htmlFor="pool-ns">
                  Namespaces (space-separated; empty gives it back to everyone)
                </label>
                <Input id="pool-ns" value={ns} onChange={(e) => setNs(e.target.value)} placeholder="team" className="mb-2 h-8 text-xs" />
                <CopyCommand args={["node", "pool", node.id, ...ns.split(/\s+/).filter(Boolean).flatMap((n) => ["--ns", n])]} label="node pool" />
              </section>
            </div>
          </>
  )
}
