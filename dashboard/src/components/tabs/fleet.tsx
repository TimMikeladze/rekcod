"use client"

import { Suspense, type ReactNode } from "react"
import { useRouter } from "next/navigation"
import { Actions } from "@/components/actions"
import { Empty, Meter, Mono, State } from "@/components/bits"
import { GetStarted, NoRemote } from "@/components/get-started"
import { nodeHealth } from "@/components/node-sheet"
import { TabRow, useTab, type TabItem } from "@/components/shell/tab-row"
import { Head, match, NavRow, SectionTitle } from "@/components/table"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, duration, fmtBytes } from "@/lib/format"
import type { NamespaceDetail, State as S } from "@/lib/types"

/** The controller's error for one of `what` (apps, jobs, nodes, ...), if that call failed. */
export function remoteError(state: S, ...what: string[]): string | null {
  const errs = state.remote?.errors ?? {}
  for (const w of what) if (errs[w]) return errs[w]
  return null
}

/** Shown instead of an empty list when the controller didn't answer. */
export function RemoteError({ error, what }: { error: string; what: string }) {
  return (
    <div role="alert" className="m-2 rounded-lg border border-alert-border bg-alert-bg p-3 text-sm">
      <div className="font-medium">Can&apos;t list {what}: the controller didn&apos;t answer</div>
      <pre className="mt-1 font-mono text-xs whitespace-pre-wrap text-muted-foreground">{error}</pre>
    </div>
  )
}

function Card({ children }: { children: ReactNode }) {
  return <div className="rounded-lg border px-3 py-2.5">{children}</div>
}

/** Nodes (a row opens `/nodes/[id]`), namespaces, routes, backups, edge bans, remotes, docker hosts. */
export function Fleet({ q = "" }: { q?: string }) {
  return (
    <Suspense>
      <FleetViews q={q} />
    </Suspense>
  )
}

/** `?view=`: one part of the fleet at a time. Parts with nothing to show have no tab. */
function FleetViews({ q }: { q: string }) {
  const { state } = useDash()
  const tabs: TabItem[] = !state
    ? [{ value: "nodes", label: "Nodes" }]
    : [
        { value: "nodes", label: "Nodes", badge: state.nodes?.length || "" },
        ...((state.namespaces.detail ?? []).length ? [{ value: "namespaces", label: "Namespaces", badge: state.namespaces.detail!.length }] : []),
        ...(state.remote?.admin ? [{ value: "edge", label: "Edge routes", badge: Object.keys(state.routes ?? {}).length || "" }, { value: "backups", label: "Backups" }] : []),
        ...(state.bans ? [{ value: "bans", label: "Bans", badge: (state.bans.detail ?? state.bans.nets).length || "" }] : []),
        { value: "remotes", label: "Remotes and hosts" },
      ]
  const view = useTab(tabs, "view")
  if (!state) return null
  return (
    <div className="space-y-6">
      <TabRow items={tabs} param="view" label="Fleet views" />
      {view === "namespaces" ? (
        <Namespaces list={state.namespaces.detail ?? []} current={state.namespaces.current} />
      ) : view === "edge" ? (
        <Routes state={state} />
      ) : view === "backups" ? (
        <Backups state={state} />
      ) : view === "bans" && state.bans ? (
        <Bans state={state} />
      ) : view === "remotes" ? (
        <div className="grid gap-6 lg:grid-cols-2 [&>*]:min-w-0">
          <Remotes state={state} />
          <Hosts state={state} />
        </div>
      ) : (
        <Nodes q={q} />
      )}
    </div>
  )
}

function Nodes({ q }: { q: string }) {
  const { state } = useDash()
  const router = useRouter()
  if (!state) return null
  const now = state.generated
  const nodesError = remoteError(state, "nodes")
  const nodes = (state.nodes ?? []).filter((n) => match(q, n.id, n.url, n.state, ...(n.pool ?? [])))
  return (
    <div className="space-y-6">
      <section>
        <SectionTitle>Nodes</SectionTitle>
        {nodesError ? (
          <RemoteError error={nodesError} what="nodes" />
        ) : state.nodes == null ? (
          state.remote ? <Empty>Listing nodes needs an admin token on the remote.</Empty> : <NoRemote what="Nodes" />
        ) : !state.nodes.length ? (
          <GetStarted title="No nodes joined" commands={[["node", "join", "http://controller:7700", "JOIN_TOKEN", "--advertise", "http://mac2:7701"]]}>
            Run this on the Mac that should take apps and jobs; the join token comes from the controller.
          </GetStarted>
        ) : !nodes.length ? (
          <Empty>Nothing matches “{q}”.</Empty>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <Head cols={["Node", ["Memory", "w-40 hidden sm:table-cell"], ["CPU", "w-32 hidden md:table-cell"], ["Disk", "w-32 hidden md:table-cell"], ["Runs", "hidden sm:table-cell"], ["Pool", "hidden lg:table-cell"], "Heartbeat", ""]} />
              <TableBody>
                {nodes.map((n) => {
                  const h = nodeHealth(n, now)
                  const total = n.mem_total_mib ?? 0
                  const used = total - (n.mem_free_mib ?? 0)
                  return (
                    <NavRow key={n.id} label={`Node ${n.id}, ${h.text}`} watch={h.text} onOpen={() => router.push(`/nodes/${encodeURIComponent(n.id)}`)}>
                      <TableCell>
                        <State tone={h.tone}>
                          <span className="font-medium">{n.id}</span>
                          <span className="text-muted-foreground">{h.text}</span>
                        </State>
                        <Mono className="block pl-3.5 text-[11px] text-muted-foreground">{n.url ?? ""}</Mono>
                      </TableCell>
                      <TableCell className="hidden sm:table-cell">
                        <div className="space-y-1">
                          <Mono>
                            {fmtBytes(used * 1024 * 1024)} / {fmtBytes(total * 1024 * 1024)}
                          </Mono>
                          <Meter value={total ? used / total : 0} />
                        </div>
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        {n.cpus ? (
                          <div className="space-y-1">
                            <Mono>
                              load {n.load?.toFixed(1) ?? "?"} / {n.cpus}
                            </Mono>
                            <Meter value={(n.load ?? 0) / n.cpus} />
                          </div>
                        ) : (
                          <Mono className="text-muted-foreground">–</Mono>
                        )}
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        {n.disk_total_gib ? (
                          <div className="space-y-1">
                            <Mono>{n.disk_free_gib} GiB free</Mono>
                            <Meter value={1 - (n.disk_free_gib ?? 0) / n.disk_total_gib} />
                          </div>
                        ) : (
                          <Mono className="text-muted-foreground">–</Mono>
                        )}
                      </TableCell>
                      <TableCell className="hidden sm:table-cell">
                        <Mono className="text-muted-foreground">
                          {n.instances ?? "?"} inst · {n.jobs_running ?? "?"} jobs
                        </Mono>
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <Mono className="text-muted-foreground">{(n.pool ?? []).join(", ") || "shared"}</Mono>
                      </TableCell>
                      <TableCell className={`text-xs ${h.stale ? "text-alert-text" : "text-muted-foreground"}`}>{ago(n.last_seen, now)}</TableCell>
                      <TableCell>
                        <Actions actions={n.actions} subject={n.id} inline={0} />
                      </TableCell>
                    </NavRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        )}
      </section>

    </div>
  )
}

function Namespaces({ list, current }: { list: NamespaceDetail[]; current: string }) {
  if (!list.length) return null
  const keys = [
    ["cpus", "CPUs", ""],
    ["memory_gib", "Memory", " GiB"],
    ["sandboxes", "Sandboxes + jobs", ""],
    ["apps", "Apps", ""],
  ] as const
  return (
    <section>
      <SectionTitle>Namespaces</SectionTitle>
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {list.map((ns) => (
          <Card key={ns.name}>
            <div className="flex items-baseline gap-2">
              <span className="font-medium">{ns.name}</span>
              {ns.name === current && <Badge variant="secondary">current</Badge>}
              <span className="ml-auto text-xs text-muted-foreground">{ns.role ? `you: ${ns.role}` : ""}</span>
            </div>
            <dl className="mt-2 space-y-1.5">
              {keys.map(([k, label, unit]) => {
                const max = ns.quota[k]
                const used = ns.used[k]
                return (
                  <div key={k} className="grid grid-cols-[1fr_auto] text-xs">
                    <dt className="text-muted-foreground">{label}</dt>
                    <dd className="font-mono">
                      {used}
                      {unit} / {max === "unlimited" ? "unlimited" : `${max}${unit}`}
                    </dd>
                    {max !== "unlimited" && (
                      <dd className="col-span-2">
                        <Meter value={max ? used / max : used ? 1 : 0} className="mt-0.5" />
                      </dd>
                    )}
                  </div>
                )
              })}
            </dl>
            <div className="mt-2 text-xs text-muted-foreground">
              {ns.name === "default"
                ? "Everyone with a token is in default."
                : ns.members.length
                  ? ns.members.map((m) => `${m.who} (${m.role})`).join(", ")
                  : "No members besides fleet admins."}
            </div>
          </Card>
        ))}
      </div>
    </section>
  )
}

function Routes({ state }: { state: S }) {
  const routes = Object.entries(state.routes ?? {})
  return (
    <section>
      <SectionTitle>Edge routes</SectionTitle>
      {state.routes == null ? (
        <Empty>The controller didn&apos;t return its route table.</Empty>
      ) : !routes.length ? (
        <Empty>No app has live or sleeping instances.</Empty>
      ) : (
        <div className="overflow-x-auto">
          <Table>
            <Head cols={["Host", "Endpoints", ["Rate", "hidden sm:table-cell"]]} />
            <TableBody>
              {routes.map(([host, r]) => (
                <TableRow key={host}>
                  <TableCell>
                    <Mono className="font-medium">{host}</Mono>
                    <div className="mt-0.5 flex gap-1">
                      <Badge variant={r.public ? "secondary" : "outline"}>{r.public ? "public" : "private"}</Badge>
                      {r.ns && r.ns !== "default" && <Badge variant="outline">ns {r.ns}</Badge>}
                    </div>
                  </TableCell>
                  <TableCell className="whitespace-normal">
                    {r.sleeping ? (
                      <State tone="idle">asleep (the edge wakes it)</State>
                    ) : (
                      <Mono className="break-all text-muted-foreground">{r.endpoints.join(", ")}</Mono>
                    )}
                  </TableCell>
                  <TableCell className="hidden sm:table-cell">
                    <Mono className="text-muted-foreground">{r.http?.rate ?? "edge default"}</Mono>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </section>
  )
}

function Backups({ state }: { state: S }) {
  const b = state.backups
  const list = [...(b?.list ?? [])].sort((x, y) => y.key.localeCompare(x.key))
  return (
    <section>
      <SectionTitle right={<Actions actions={state.controller_actions ?? []} subject="controller" inline={1} />}>Controller backups</SectionTitle>
      {b?.error ? (
        <div className="m-2 rounded-md border px-3 py-2 text-xs text-muted-foreground">
          <div>No backups: {b.error}</div>
          <div className="mt-1">The controller backs up hourly once it runs with `--storage`.</div>
        </div>
      ) : !list.length ? (
        <Empty>No backups yet.</Empty>
      ) : (
        <Table>
          <Head cols={["Backup", ["Size", "text-right"], "Taken"]} />
          <TableBody>
            {list.map((x) => (
              <TableRow key={x.key}>
                <TableCell className="max-w-48 truncate">
                  <Mono>{x.key}</Mono>
                </TableCell>
                <TableCell className="text-right">
                  <Mono>{fmtBytes(x.size)}</Mono>
                </TableCell>
                <TableCell className="text-xs text-muted-foreground">{ago(Date.parse(x.modified) / 1000, state.generated)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </section>
  )
}

function Bans({ state }: { state: S }) {
  const bans = state.bans!
  const detail = new Map((bans.detail ?? []).map((d) => [d.net, d]))
  const now = state.generated
  const clients = bans.clients ?? []
  const banAction = (ip: string) => bans.actions.filter((a) => a.kind === "EdgeBan" && a.argv?.length === 3 && a.argv[2] === ip)
  return (
    <section className="space-y-4">
      <div>
        <SectionTitle right={<Actions actions={bans.actions.filter((a) => a.kind === "EdgeBan" && a.interactive)} subject="edge" inline={1} />}>
          Edge bans
        </SectionTitle>
        {bans.nets.length ? (
          <div className="overflow-x-auto">
            <Table>
              <Head cols={["Address", ["Reason", "hidden sm:table-cell"], "Added", ["Expires", "hidden sm:table-cell"], ""]} />
              <TableBody>
                {bans.nets.map((n) => {
                  const d = detail.get(n)
                  return (
                    <TableRow key={n}>
                      <TableCell>
                        <Mono className="font-medium">{n}</Mono>
                        {d?.reason && <div className="text-xs text-muted-foreground sm:hidden">{d.reason}</div>}
                      </TableCell>
                      <TableCell className="hidden text-xs sm:table-cell">{d?.reason || <span className="text-muted-foreground">–</span>}</TableCell>
                      <TableCell className="text-xs text-muted-foreground">
                        {d ? `${ago(d.created, now)}${d.by ? ` by ${d.by}` : ""}` : "–"}
                      </TableCell>
                      <TableCell className="hidden text-xs text-muted-foreground sm:table-cell">
                        {d ? (d.until ? (d.until > now ? `in ${duration(d.until - now)}` : "expired") : "never") : "–"}
                      </TableCell>
                      <TableCell>
                        <Actions actions={bans.actions.filter((a) => a.kind === "EdgeUnban" && a.argv?.at(-1) === n)} subject={n} inline={1} />
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        ) : (
          <Empty>Nothing banned.</Empty>
        )}
      </div>
      <div>
        <SectionTitle>Busiest clients, last 10 minutes</SectionTitle>
        {!clients.length ? (
          <Empty>The edges haven&apos;t reported any clients.</Empty>
        ) : (
          <Table>
            <Head cols={["Client", ["Requests", "text-right"], ["Rate-limited", "text-right"], ""]} />
            <TableBody>
              {clients.map((c) => (
                <TableRow key={c.ip}>
                  <TableCell>
                    <Mono className="font-medium">{c.ip}</Mono>
                  </TableCell>
                  <TableCell className="text-right">
                    <Mono>{c.requests}</Mono>
                  </TableCell>
                  <TableCell className="text-right">
                    <Mono className={c.limited ? "text-alert-text" : "text-muted-foreground"}>{c.limited}</Mono>
                  </TableCell>
                  <TableCell>
                    {banAction(c.ip).length ? (
                      <Actions actions={banAction(c.ip)} subject={c.ip} inline={1} />
                    ) : (
                      <span className="text-xs text-muted-foreground">banned</span>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>
    </section>
  )
}

function Remotes({ state }: { state: S }) {
  const r = state.remotes
  if (!r?.all.length) return null
  return (
    <section>
      <SectionTitle>Remotes</SectionTitle>
      <Table>
        <Head cols={["Remote", "Default", ""]} />
        <TableBody>
          {r.all.map((name) => (
            <TableRow key={name}>
              <TableCell>
                <span className="font-medium">{name}</span>
                {name === r.default && state.remote?.url && <Mono className="block text-[11px] text-muted-foreground">{state.remote.url}</Mono>}
              </TableCell>
              <TableCell>{name === r.default ? <State tone="ok">default</State> : null}</TableCell>
              <TableCell>
                <Actions actions={r.actions.filter((a) => a.argv?.at(-1) === name)} subject={name} inline={1} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </section>
  )
}

function Hosts({ state }: { state: S }) {
  return (
    <section>
      <SectionTitle>Docker hosts</SectionTitle>
      <Table>
        <Head cols={["Host", ["Endpoint", "hidden sm:table-cell"], "In use", ""]} />
        <TableBody>
          <TableRow>
            <TableCell className="font-medium">local</TableCell>
            <TableCell className="hidden sm:table-cell">
              <Mono className="text-muted-foreground">this Mac&apos;s rekcod VM</Mono>
            </TableCell>
            <TableCell>{state.hosts.local_current ? <State tone="ok">current</State> : null}</TableCell>
            <TableCell>
              <Actions actions={state.hosts.actions.filter((a) => a.argv?.at(-1) === "local")} subject="local" />
            </TableCell>
          </TableRow>
          {state.hosts.all.map((h) => (
            <TableRow key={h.name}>
              <TableCell className="font-medium">{h.name}</TableCell>
              <TableCell className="hidden sm:table-cell">
                <Mono className="text-muted-foreground">{h.endpoint}</Mono>
              </TableCell>
              <TableCell>{h.current ? <State tone="ok">current</State> : null}</TableCell>
              <TableCell>
                <Actions actions={state.hosts.actions.filter((a) => a.argv?.at(-1) === h.name)} subject={h.name} inline={2} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </section>
  )
}
