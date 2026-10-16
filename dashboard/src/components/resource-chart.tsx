"use client"

// The only module that imports @tanstack/charts for the local views (it pulls in d3); the rest
// of the app imports ResourceChart, FsChart and WindowToggle from here.
import { Component, useMemo, type ReactNode } from "react"
import { defineChart, lineY } from "@tanstack/charts"
import { Chart, type ChartPoint } from "@tanstack/charts/react"
import { colorLegend, colorLegendItems } from "@tanstack/charts/legend"
import { scaleLinear } from "@tanstack/charts/scales/linear"
import { scaleOrdinal } from "@tanstack/charts/scales/ordinal"
import { tooltip } from "@tanstack/charts/tooltip"
import { scaleUtc } from "d3-scale"
import { useDash, WINDOWS, type Window } from "@/hooks/dash-context"
import { fmtBytes } from "@/lib/format"
import type { Sample } from "@/lib/types"
import { cn } from "@/lib/utils"

/** Compose and workerd names run past 60 characters; the legend gets the first 24. */
const clip = (name: string) => (name.length > 24 ? `${name.slice(0, 23)}…` : name)

const OTHER = "#7a7f87"
const FALLBACK = "#2a78d6"

const WINDOW_LABEL: Record<Window, string> = { 300: "5m", 900: "15m", 3600: "1h" }
const windowWords = (w: Window) => (w === 300 ? "five minutes" : w === 900 ? "fifteen minutes" : "hour")

const timeFormat = (d: Date) => d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })

/**
 * CPU or memory per container over the chart window. The busiest `top` containers get their
 * own line, the rest are summed into "other". With `name`, just that container.
 * Colours come from the server's per-name map, so a container keeps its colour everywhere.
 */
export function ResourceChart({ metric, name, top = 5, height = 180 }: { metric: "cpu" | "mem"; name?: string; top?: number; height?: number }) {
  const { history, colors, window } = useDash()
  const rows = useMemo(() => {
    if (name) return history.filter((s) => s.name === name)
    const latest = new Map<string, Sample>()
    for (const s of history) latest.set(s.name, s)
    const names = new Set([...latest.values()].sort((a, b) => b[metric] - a[metric]).slice(0, top).map((s) => s.name))
    const out: Sample[] = []
    const other = new Map<number, Sample>()
    for (const s of history) {
      if (names.has(s.name)) out.push(s)
      else {
        const o = other.get(s.t) ?? { t: s.t, name: "other", cpu: 0, mem: 0 }
        o.cpu += s.cpu
        o.mem += s.mem
        other.set(s.t, o)
      }
    }
    return [...out, ...other.values()].sort((a, b) => a.t - b.t)
  }, [history, metric, top, name])

  const definition = useMemo(() => {
    // Domain: the names drawn, sorted; range: each one's own colour. Neither depends on order seen.
    const domain = [...new Set(rows.map((r) => r.name))].sort()
    const range = domain.map((n) => colors[n] ?? (n === "other" ? OTHER : FALLBACK))
    return defineChart({
      marks: [
        lineY(rows, {
          x: (d: Sample) => new Date(d.t),
          y: (d: Sample) => d[metric],
          color: (d: Sample) => clip(d.name),
          strokeWidth: 1.5,
        }),
      ],
      scales: {
        x: { scale: scaleUtc, axis: { ticks: { format: timeFormat } } },
        y: {
          scale: scaleLinear,
          nice: true,
          grid: true,
          axis: {
            label: metric === "cpu" ? "CPU %" : "Memory",
            ticks: { format: (v: number) => (metric === "mem" ? fmtBytes(v) : `${v}%`) },
          },
        },
      },
      color: {
        scale: () => scaleOrdinal<string, string>(domain.map(clip), range),
        // Without it the factory's domain is re-inferred in data order and names swap colours.
        domain: domain.map(clip),
        ...(name ? {} : { legend: colorLegend({ placement: "bottom", itemWidth: 170, items: colorLegendItems({ gap: 8, rowGap: 4 }) }) }),
      },
      tooltip: {
        use: tooltip,
        format: (p: ChartPoint<Sample>) =>
          `${p.datum.name} · ${metric === "cpu" ? `${p.datum.cpu.toFixed(1)}%` : fmtBytes(p.datum.mem)} · ${timeFormat(new Date(p.datum.t))}`,
      },
    })
  }, [rows, metric, colors, name])

  const points = new Set(rows.map((r) => r.t)).size
  if (points < 2) {
    return (
      <div className="flex items-center justify-center text-xs text-muted-foreground" style={{ height }}>
        {history.length ? (name ? "no samples for this container yet" : "no running containers") : "collecting samples…"}
      </div>
    )
  }
  const what = metric === "cpu" ? "CPU use" : "Memory use"
  return (
    <ChartBoundary height={height}>
      <Chart
        definition={definition}
        ariaLabel={name ? `${what} of ${name} over the last ${windowWords(window)}` : `${what} per container over the last ${windowWords(window)}`}
        height={height}
      />
    </ChartBoundary>
  )
}

/** Every container's CPU and memory added up at one sample time (ms). */
export type Total = { t: number; cpu: number; mem: number }

/** Sums samples per time; a total has no single original row, so this is the row. */
export function totals(history: Sample[]): Total[] {
  const by = new Map<number, Total>()
  for (const s of history) {
    const o = by.get(s.t) ?? { t: s.t, cpu: 0, mem: 0 }
    o.cpu += s.cpu
    o.mem += s.mem
    by.set(s.t, o)
  }
  return [...by.values()].sort((a, b) => a.t - b.t)
}

/** The overview tile's line: all containers' CPU or memory over the chart window, no axes. */
export function UsageSpark({ metric, height = 28 }: { metric: "cpu" | "mem"; height?: number }) {
  const { history, window } = useDash()
  const rows = useMemo(() => totals(history), [history])
  const definition = useMemo(
    () =>
      defineChart({
        marks: [
          lineY(rows, { x: (d: Total) => new Date(d.t), y: (d: Total) => d[metric], stroke: "var(--chart-ink)", strokeWidth: 1.25 }),
        ],
        scales: { x: { scale: scaleUtc }, y: { scale: scaleLinear } },
        guides: false,
        margin: 2,
        tooltip: {
          use: tooltip,
          format: (p: ChartPoint<Total>) => `${metric === "cpu" ? `${p.datum.cpu.toFixed(1)}%` : fmtBytes(p.datum.mem)} · ${timeFormat(new Date(p.datum.t))}`,
        },
      }),
    [rows, metric],
  )
  if (rows.length < 2) return <div style={{ height }} />
  return (
    <ChartBoundary height={height}>
      <Chart definition={definition} ariaLabel={`Total ${metric === "cpu" ? "CPU" : "memory"} use of all containers over the last ${windowWords(window)}`} height={height} />
    </ChartBoundary>
  )
}

/** 5m / 15m / 1h for both resource charts; the server keeps an hour. */
export function WindowToggle({ className }: { className?: string }) {
  const { window, setWindow } = useDash()
  return (
    <div role="group" aria-label="Chart window" className={cn("inline-flex rounded-md border p-0.5", className)}>
      {WINDOWS.map((w) => (
        <button
          key={w}
          type="button"
          aria-pressed={w === window}
          onClick={() => setWindow(w)}
          className={cn(
            "rounded px-2 py-0.5 font-mono text-[11px] text-muted-foreground transition-colors hover:text-foreground",
            w === window && "bg-muted text-foreground",
          )}
        >
          {WINDOW_LABEL[w]}
        </button>
      ))}
    </div>
  )
}

/** One virtio-fs operation's requests per second at time `t`. */
export type FsRate = { t: number; op: string; rate: number }

const FS_PALETTE = ["#2a78d6", "#d9822b", "#2e9e6b", "#b8459a", "#8a6fd1"]

/** File-sharing requests per second for the busiest operations. Rows come from `fsRates`. */
export function FsChart({ rows, height = 150 }: { rows: FsRate[]; height?: number }) {
  const definition = useMemo(() => {
    const domain = [...new Set(rows.map((r) => r.op))]
    return defineChart({
      marks: [lineY(rows, { x: (d: FsRate) => new Date(d.t), y: (d: FsRate) => d.rate, color: (d: FsRate) => d.op, strokeWidth: 1.5 })],
      scales: {
        x: { scale: scaleUtc, axis: { ticks: { format: timeFormat } } },
        y: { scale: scaleLinear, nice: true, grid: true, axis: { label: "requests/s", ticks: { format: (v: number) => `${v}` } } },
      },
      color: {
        scale: () => scaleOrdinal<string, string>(domain, FS_PALETTE),
        domain,
        legend: colorLegend({ placement: "bottom", itemWidth: 110, items: colorLegendItems({ gap: 8, rowGap: 4 }) }),
      },
      tooltip: { use: tooltip, format: (p: ChartPoint<FsRate>) => `${p.datum.op} · ${p.datum.rate.toFixed(1)}/s · ${timeFormat(new Date(p.datum.t))}` },
    })
  }, [rows])
  if (new Set(rows.map((r) => r.t)).size < 2) {
    return (
      <div className="flex items-center justify-center text-xs text-muted-foreground" style={{ height }}>
        collecting samples…
      </div>
    )
  }
  return (
    <ChartBoundary height={height}>
      <Chart definition={definition} ariaLabel="File-sharing requests per second by operation" height={height} />
    </ChartBoundary>
  )
}

/** A chart that throws shows a note instead of taking the page down. */
class ChartBoundary extends Component<{ children: ReactNode; height: number }, { error: string | null }> {
  state = { error: null as string | null }
  static getDerivedStateFromError(e: Error) {
    return { error: e.message }
  }
  render() {
    if (this.state.error)
      return (
        <div className="flex items-center justify-center text-xs text-alert-text" style={{ height: this.props.height }}>
          chart failed: {this.state.error}
        </div>
      )
    return this.props.children
  }
}
