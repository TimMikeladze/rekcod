// Turning recorded samples into chart rows without inventing readings. No chart imports, so the
// tests import it directly.
import type { TrafficMinute } from "@/lib/types"

/** A traffic row, or a minute the edge didn't report (no requests, so no latency either). */
export type Minute = Omit<TrafficMinute, "p50_ms" | "p95_ms"> & { p50_ms: number | null; p95_ms: number | null }

/**
 * Edges only report minutes that had requests. Between the first and last reported minute, an
 * absent one is 0 requests; without it the line would interpolate traffic that never happened.
 */
export function fillMinutes(rows: TrafficMinute[]): Minute[] {
  const out: Minute[] = []
  for (const r of [...rows].sort((a, b) => a.minute - b.minute)) {
    const last = out.at(-1)?.minute
    if (last != null) for (let m = last + 1; m < r.minute; m++) out.push({ minute: m, requests: 0, errors: 0, p50_ms: null, p95_ms: null })
    out.push(r)
  }
  return out
}

/**
 * Inserts `gap(prev)` wherever one series (`key`) goes more than `maxMs` without a sample, so a
 * line breaks there instead of drawing across time nobody measured. `gap` returns a row whose
 * plotted value isn't finite (null or NaN); lineY splits on it. Rows must be sorted by `t`.
 */
export function breakGaps<T extends { t: number }>(rows: T[], maxMs: number, gap: (prev: T) => T, key: (r: T) => string = () => ""): T[] {
  const last = new Map<string, number>()
  const out: T[] = []
  for (const r of rows) {
    const k = key(r)
    const prev = last.get(k)
    if (prev != null && r.t - out[prev].t > maxMs) out.push(gap(out[prev]))
    last.set(k, out.length)
    out.push(r)
  }
  return out
}
