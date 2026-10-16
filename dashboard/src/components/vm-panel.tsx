"use client"

import { useMemo } from "react"
import Link from "next/link"
import { Actions } from "@/components/actions"
import { Empty, State } from "@/components/bits"
import { DetailHead } from "@/components/shell/page-title"
import { FsChart, type FsRate } from "@/components/resource-chart"
import { useDash } from "@/hooks/dash-context"
import { duration, fmtBytes } from "@/lib/format"

/** FUSE opcodes the guest counts (`fs_requests` keys). */
const FUSE_OPS: Record<string, string> = {
  "1": "LOOKUP",
  "3": "GETATTR",
  "4": "SETATTR",
  "14": "OPEN",
  "15": "READ",
  "16": "WRITE",
  "18": "RELEASE",
  "22": "GETXATTR",
  "27": "OPENDIR",
  "28": "READDIR",
  "29": "RELEASEDIR",
  "35": "CREATE",
  "44": "READDIRPLUS",
  "52": "STATX",
}
export const fuseOp = (code: string) => FUSE_OPS[code] ?? `op ${code}`

/**
 * Requests per second between consecutive counter samples, for the `top` busiest operations.
 * A counter that went down (the VM restarted) skips that interval.
 */
export function fsRates(history: { t: number; ops: Record<string, number> }[], top = 4): FsRate[] {
  const all: FsRate[] = []
  const total = new Map<string, number>()
  for (let i = 1; i < history.length; i++) {
    const [a, b] = [history[i - 1], history[i]]
    const dt = (b.t - a.t) / 1000
    if (dt <= 0) continue
    for (const [k, v] of Object.entries(b.ops)) {
      const d = v - (a.ops[k] ?? 0)
      if (d < 0) continue
      const op = fuseOp(k)
      all.push({ t: b.t, op, rate: d / dt })
      total.set(op, (total.get(op) ?? 0) + d)
    }
  }
  const keep = new Set([...total].sort((x, y) => y[1] - x[1]).slice(0, top).map(([op]) => op))
  return all.filter((r) => keep.has(r.op))
}

function Row({ k, children }: { k: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="text-muted-foreground">{k}</dt>
      <dd className="min-w-0 font-mono text-xs break-all">{children}</dd>
    </>
  )
}

function useVm() {
  const { state, fsHistory } = useDash()
  const rows = useMemo(() => fsRates(fsHistory), [fsHistory])
  return { state, rows }
}

/** The facts, as a definition list. */
function Facts() {
  const { state } = useDash()
  if (!state) return null
  const st = state.status
  const sb = state.sandbox_vm
  // `uptime_secs` comes from `rekcod state` (ps etime of the VM process).
  const uptime = (st as { uptime_secs?: number }).uptime_secs
  const warming = Array.isArray(st.warming) ? (st.warming as string[]) : []
  return (
    <dl className="facts h-fit grid-cols-[8rem_1fr]">
      <Row k="Uptime">{uptime ? duration(uptime) : "–"}</Row>
      <Row k="Kernel">{st.kernel ?? "–"}</Row>
      <Row k="dockerd">
        {st.dockerd ?? "?"} · API {st.api ?? "?"}
      </Row>
      <Row k="Resources">
        {st.cpus ?? "?"} CPU · {fmtBytes((st.memory_mib ?? 0) * 1024 * 1024)}
      </Row>
      <Row k="File cache">{st.fs_cache ? "on" : "off"}</Row>
      <Row k="Warming">{warming.length ? warming.join(", ") : "idle"}</Row>
      <Row k="docker.sock">{st.default_socket ?? "–"}</Row>
      <Row k="Context">{st.context ?? "–"}</Row>
      <Row k="Socket">{st.socket ?? "–"}</Row>
      <Row k="Sandbox VM">
        {sb ? (
          <State tone={sb.running ? "ok" : "idle"}>{sb.running ? `running${sb.project ? ` · ${sb.project}` : ""}` : "stopped"}</State>
        ) : (
          "not running"
        )}
      </Row>
    </dl>
  )
}

/** `/vm`: versions, settings, the sandbox VM, and file-sharing requests per second. */
export function VmDetail() {
  const { state, rows } = useVm()
  return (
    <div className="space-y-6 text-sm">
      <DetailHead title="VM" sub={state?.status.socket}>
        {state && <Actions actions={state.vm_actions.filter((a) => a.kind !== "DashboardOpen")} subject="rekcod" inline={2} toolbar />}
      </DetailHead>
      {!state ? (
        <Empty>Loading…</Empty>
      ) : !state.status.running ? (
        <State tone="idle">The VM is stopped.</State>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[minmax(0,24rem)_1fr]">
          <section>
            <h2 className="h2 mb-3">Settings</h2>
            <Facts />
          </section>
          <section className="min-w-0">
            <h2 className="h2 mb-3">File-sharing requests per second</h2>
            <FsChart rows={rows} height={220} />
          </section>
        </div>
      )}
    </div>
  )
}

/** A compact VM box for the overview; the title links to `/vm`. */
export function VmPanel() {
  const { state, rows } = useVm()
  if (!state) return null
  return (
    <section className="rounded-lg border p-3" aria-labelledby="vm-panel-title">
      <h2 id="vm-panel-title" className="mb-2 text-xs font-medium text-muted-foreground">
        <Link href="/vm" className="hover:text-foreground hover:underline">
          VM
        </Link>
      </h2>
      {!state.status.running ? (
        <State tone="idle">stopped</State>
      ) : (
        <div className="grid gap-4 md:grid-cols-[minmax(0,20rem)_1fr]">
          <Facts />
          <div className="min-w-0">
            <div className="mb-1 text-[11px] text-muted-foreground">File-sharing requests per second</div>
            <FsChart rows={rows} />
          </div>
        </div>
      )}
    </section>
  )
}
