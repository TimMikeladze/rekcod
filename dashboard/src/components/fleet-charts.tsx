"use client"

// The only module of the apps/jobs/fleet views that imports @tanstack/charts (it pulls in
// d3); app-sheet.tsx and job-sheet.tsx import their charts from here.
import { Component, useMemo, type ReactNode } from "react"
import { areaY, defineChart, dot, lineY, rect } from "@tanstack/charts"
import { Chart, type ChartPoint } from "@tanstack/charts/react"
import { colorLegend, colorLegendItems } from "@tanstack/charts/legend"
import { scaleBand } from "@tanstack/charts/scales/band"
import { scaleLinear } from "@tanstack/charts/scales/linear"
import { scaleOrdinal } from "@tanstack/charts/scales/ordinal"
import { tooltip } from "@tanstack/charts/tooltip"
import { scaleUtc } from "d3-scale"
import { duration } from "@/lib/format"
import { breakGaps, fillMinutes, type Minute } from "@/lib/series"
import type { Job, TrafficMinute } from "@/lib/types"

const clock = (d: Date) => d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })
const day = (d: Date) => d.toLocaleDateString([], { month: "short", day: "numeric" })
const at = (m: Minute) => new Date(m.minute * 60_000)


// Categorical slots 1 and 2 of the reference palette; run results use the status colours.
const SERIES = ["#2a78d6", "#eb6834"]
const legend = () => colorLegend({ placement: "bottom", itemWidth: 120, items: colorLegendItems({ gap: 8, rowGap: 4 }) })

/**
 * One count per minute over the last hour (requests, or 5xx). Separate charts rather than one:
 * a few errors next to hundreds of requests would sit flat on the baseline.
 */
export function PerMinuteChart({ rows, y, label, color, ariaLabel }: { rows: Minute[]; y: (d: Minute) => number; label: string; color: string; ariaLabel: string }) {
  const definition = useMemo(
    () =>
      defineChart({
        marks: [lineY(rows, { x: at, y, color: () => label, strokeWidth: 2 })],
        scales: {
          x: { scale: scaleUtc, axis: { ticks: { format: clock } } },
          y: { scale: scaleLinear, nice: true, grid: true, axis: { label: `${label} / min` } },
        },
        color: { scale: () => scaleOrdinal<string, string>([label], [color]), domain: [label] },
        tooltip: {
          use: tooltip,
          format: (p: ChartPoint<Minute>) => `${clock(at(p.datum))} · ${y(p.datum)} ${label}`,
        },
      }),
    [rows, y, label, color],
  )
  return (
    <Boundary>
      <Chart definition={definition} ariaLabel={ariaLabel} height={120} />
    </Boundary>
  )
}

const requests = (d: Minute) => d.requests
const errors = (d: Minute) => d.errors

/** Requests per minute, then 5xx per minute, over the last hour. */
export function TrafficChart({ rows: raw }: { rows: TrafficMinute[] }) {
  const rows = useMemo(() => fillMinutes(raw), [raw])
  return (
    <div className="space-y-2">
      <PerMinuteChart rows={rows} y={requests} label="requests" color={SERIES[0]} ariaLabel="Requests per minute over the last hour" />
      <PerMinuteChart rows={rows} y={errors} label="5xx" color="var(--alert)" ariaLabel="5xx responses per minute over the last hour" />
    </div>
  )
}

/** p50 and p95 latency per minute. */
export function LatencyChart({ rows: raw }: { rows: TrafficMinute[] }) {
  const rows = useMemo(() => fillMinutes(raw), [raw])
  const definition = useMemo(
    () =>
      defineChart({
        marks: [
          lineY(rows, { x: at, y: (d: Minute) => d.p50_ms, color: () => "p50", strokeWidth: 2 }),
          lineY(rows, { x: at, y: (d: Minute) => d.p95_ms, color: () => "p95", strokeWidth: 2 }),
        ],
        scales: {
          x: { scale: scaleUtc, axis: { ticks: { format: clock } } },
          y: { scale: scaleLinear, nice: true, grid: true, axis: { label: "ms", ticks: { format: (v: number) => `${v} ms` } } },
        },
        color: { scale: () => scaleOrdinal<string, string>(["p50", "p95"], SERIES), domain: ["p50", "p95"], legend: legend() },
        tooltip: {
          use: tooltip,
          format: (p: ChartPoint<Minute>) => `${clock(at(p.datum))} · p50 ${p.datum.p50_ms} ms · p95 ${p.datum.p95_ms} ms`,
        },
      }),
    [rows],
  )
  return (
    <Boundary>
      <Chart definition={definition} ariaLabel="Median and 95th percentile response time per minute over the last hour" height={160} />
    </Boundary>
  )
}

/** done / failed / other, for a finished run. */
export function runResult(j: Job): string {
  if (j.state === "done" && (j.exit ?? 0) === 0) return "succeeded"
  if (j.state === "failed" || j.state === "lost" || (j.exit != null && j.exit !== 0)) return "failed"
  return j.state
}

const took = (j: Job) => (j.finished && j.created != null ? Math.max(1, j.finished - j.created) : null)

/** Duration of each finished run, coloured by how it ended. */
export function RunHistoryChart({ runs }: { runs: Job[] }) {
  const finished = useMemo(() => runs.filter((j) => j.finished && j.created != null), [runs])
  const definition = useMemo(
    () =>
      defineChart({
        marks: [
          dot(finished, {
            x: (d: Job) => new Date(d.created! * 1000),
            y: (d: Job) => took(d),
            color: (d: Job) => runResult(d),
            r: 4,
          }),
        ],
        scales: {
          x: { scale: scaleUtc, axis: { ticks: { format: day } } },
          y: { scale: scaleLinear, nice: true, grid: true, axis: { label: "took", ticks: { format: (v: number) => (v ? duration(v) : "0s") } } },
        },
        color: {
          scale: () => scaleOrdinal<string, string>(["succeeded", "failed", "cancelled"], ["var(--success)", "var(--alert)", "var(--strong)"]),
          // A factory's domain is otherwise re-inferred in data order, which reshuffles the colours.
          domain: ["succeeded", "failed", "cancelled"],
          legend: legend(),
        },
        tooltip: {
          use: tooltip,
          format: (p: ChartPoint<Job>) =>
            `${p.datum.id} · ${runResult(p.datum)}${p.datum.exit != null ? ` (exit ${p.datum.exit})` : ""} · ${duration(took(p.datum) ?? 0)}`,
        },
      }),
    [finished],
  )
  if (finished.length < 2) return <p className="text-xs text-muted-foreground">Not enough finished runs to chart yet.</p>
  return (
    <Boundary>
      <Chart definition={definition} ariaLabel={`Duration and result of the last ${finished.length} runs`} height={160} />
    </Boundary>
  )
}

/** Requests per minute over the last hour as a 24 px line with no axes, for a table cell. */
export function TrafficSpark({ rows: raw, app }: { rows: TrafficMinute[]; app: string }) {
  const rows = useMemo(() => fillMinutes(raw), [raw])
  const definition = useMemo(
    () =>
      defineChart({
        marks: [
          lineY(rows, { x: at, y: requests, stroke: "var(--chart-ink)", strokeWidth: 1.25 }),
        ],
        scales: { x: { scale: scaleUtc }, y: { scale: scaleLinear } },
        guides: false,
        margin: 2,
        tooltip: { use: tooltip, format: (p: ChartPoint<Minute>) => `${clock(at(p.datum))} · ${p.datum.requests} req · ${p.datum.errors} 5xx` },
      }),
    [rows],
  )
  if (rows.length < 2) return <span className="text-xs text-muted-foreground">–</span>
  return (
    <Boundary>
      <Chart definition={definition} ariaLabel={`Requests per minute to ${app} over the last hour`} height={24} />
    </Boundary>
  )
}

/** One job run on the timeline: its row, plus the lane and end the chart draws. */
const lane = (j: Job) => j.name ?? j.schedule ?? "one-off"
const RESULTS = ["succeeded", "failed", "running", "cancelled"]
const RESULT_COLORS = ["var(--success)", "var(--alert)", "var(--ok)", "var(--strong)"]

/**
 * The last 24 hours of runs: one lane per job name, a bar from start to finish (to `now` while
 * it runs) and a dot at the start, so a 30 s run in a day-wide chart still shows.
 */
export function JobsTimeline({ jobs, now }: { jobs: Job[]; now: number }) {
  // A run without a start time has no place on a timeline.
  const rows = useMemo(() => jobs.filter((j): j is Job & { created: number } => j.created != null && j.created > now - 86_400), [jobs, now])
  const lanes = useMemo(() => [...new Set(rows.map(lane))].sort(), [rows])
  const definition = useMemo(() => {
    const end = (j: Job & { created: number }) => new Date(1000 * (j.finished ?? (j.state === "running" ? now : j.created)))
    const result = (j: Job) => (j.state === "running" ? "running" : runResult(j))
    return defineChart({
      marks: [
        rect(rows, { x1: (d: Job & { created: number }) => new Date(d.created * 1000), x2: end, y: lane, color: result, inset: 3, radius: 2 }),
        dot(rows, { x: (d: Job & { created: number }) => new Date(d.created * 1000), y: lane, color: result, r: 3.5 }),
      ],
      scales: {
        // Instances keep their domains: the whole day, every lane.
        x: { scale: scaleUtc().domain([new Date((now - 86_400) * 1000), new Date(now * 1000)]), grid: true, axis: { ticks: { format: clock } } },
        y: { scale: scaleBand<string>(lanes, [0, 1]).padding(0.25), axis: { label: "" } },
      },
      color: { scale: () => scaleOrdinal<string, string>(RESULTS, RESULT_COLORS), domain: RESULTS, legend: legend() },
      tooltip: {
        use: tooltip,
        format: (p: ChartPoint<Job & { created: number }>) =>
          `${lane(p.datum)} · ${p.datum.id} · ${result(p.datum)} · ${p.datum.finished ? duration(Math.max(1, p.datum.finished - p.datum.created)) : "running"}`,
      },
    })
  }, [rows, lanes, now])
  if (!rows.length) return <p className="text-xs text-muted-foreground">No runs in the last 24 hours.</p>
  return (
    <Boundary>
      <Chart definition={definition} ariaLabel={`Job runs in the last 24 hours, one lane per job: ${lanes.join(", ")}`} height={Math.max(110, 36 * lanes.length + 60)} />
    </Boundary>
  )
}

/** One node at time `t` (ms): load, memory used and disk used, from a state the server read. */
export type NodeSample = { t: number; load: number | null; mem_used_mib: number | null; disk_used_gib: number | null }

const NODE_METRIC = {
  load: { label: "load", fmt: (v: number) => v.toFixed(1), y: (d: NodeSample) => d.load },
  mem: { label: "memory used", fmt: (v: number) => `${+v.toFixed(1)} GiB`, y: (d: NodeSample) => (d.mem_used_mib == null ? null : d.mem_used_mib / 1024) },
  disk: { label: "disk used", fmt: (v: number) => `${v.toFixed(0)} GiB`, y: (d: NodeSample) => d.disk_used_gib },
} as const

/** A node's load, memory or disk over the last hour. `max` (CPUs, total memory or disk) pins the top. */
export function NodeHistoryChart({ rows: raw, metric, max, node }: { rows: NodeSample[]; metric: keyof typeof NODE_METRIC; max?: number; node: string }) {
  const m = NODE_METRIC[metric]
  // A node is unreachable after a minute without a heartbeat; don't draw across that.
  const rows = useMemo(() => breakGaps(raw, 60_000, (p) => ({ t: p.t + 1, load: null, mem_used_mib: null, disk_used_gib: null })), [raw])
  const definition = useMemo(
    () =>
      defineChart({
        marks: [
          areaY(rows, { x: (d: NodeSample) => new Date(d.t), y: m.y, fill: "var(--chart-area)" }),
          lineY(rows, { x: (d: NodeSample) => new Date(d.t), y: m.y, stroke: "var(--chart-ink)", strokeWidth: 1.5 }),
        ],
        scales: {
          x: { scale: scaleUtc, axis: { ticks: { format: clock } } },
          // An instance keeps its domain (a factory's is inferred from the data): 0 to the node's size.
          y: { scale: max ? scaleLinear().domain([0, max]) : scaleLinear, nice: true, grid: true, axis: { ticks: { format: m.fmt } } },
        },
        tooltip: { use: tooltip, format: (p: ChartPoint<NodeSample>) => `${clock(new Date(p.datum.t))} · ${m.label} ${m.y(p.datum) == null ? "–" : m.fmt(m.y(p.datum)!)}` },
      }),
    [rows, m, max],
  )
  if (rows.filter((r) => m.y(r) != null).length < 2) return <div className="flex h-[110px] items-center justify-center text-xs text-muted-foreground">collecting samples…</div>
  return (
    <Boundary>
      <Chart definition={definition} ariaLabel={`${m.label} of node ${node} over the last hour`} height={110} />
    </Boundary>
  )
}

/** A chart that throws shows a note instead of taking the sheet down. */
class Boundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null }
  static getDerivedStateFromError(e: Error) {
    return { error: e.message }
  }
  render() {
    if (this.state.error) return <div className="flex h-[160px] items-center justify-center text-xs text-alert-text">chart failed: {this.state.error}</div>
    return this.props.children
  }
}
